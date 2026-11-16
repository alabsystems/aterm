// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE LIVENESS READING OF A CLAUDE CODE LOGIN AND THE UPKEEP STEP THAT ADOPTS IT (plan
//! package FL-174).
//!
//! A row's recorded sign-out is a belief: a session met a wall, and the accounts layer wrote
//! `signed out`. What a person does next in that tab (`/login`, then a served turn) is a fact,
//! and until this module only a tab's own supervisor recorded it, so a tab with no supervisor
//! (Autopilot off, a worker that stopped) left the belief standing for hours while the upkeep
//! opened sign-in windows for an account that was serving.
//!
//! [`Witnesses::live_login_here`] is the one reading. It reads files that hold no credential and
//! nothing else, never the network, a process or the keychain:
//!
//! - a tab's usage-tap file (`rl/<sid>-<pid>.json`, [`super::tap`]), written by Claude Code's
//!   status line whether or not anything supervises the tab. It names the process's login
//!   place ([`tap::Route::Stored`], the default home, the row's login place) and the
//!   conversation it holds, and this module keeps a baseline of it taken after the sign-out
//!   ([`Baselines`], kept on disk so a restart does not turn old growth into new evidence);
//! - the transcript of the conversation such a file names, read from its end
//!   ([`Conversation`]): its `/login` rows that succeeded, and its served requests, each a
//!   prompt or tool result with the first row of a NEW API response chained to it.
//!
//! SIGNED-IN evidence is a `/login` that succeeded in that conversation after the sign-out and
//! after the baseline. SERVED evidence is a request that BEGAN after both (its prompt or tool
//! result is dated after them) and was answered by a response that is not the one already in
//! flight when that row was written (Claude Code runs a tool while its response still streams,
//! so a tool's result is followed by later rows of the same response), alongside API time
//! grown past the baseline with a rate-limit figure. Growth alone is no evidence: a request
//! authorized before the sign-out ends after it. Neither counts once the conversation shows a
//! login wall after it, from a file its process stopped writing before the evidence's time, or
//! for a row whose login place no file without a credential proves the row's own
//! ([`Witnesses::names_row`]): the default login's profile must name the row's address, and a
//! row with a login directory of its own is proved by nothing here and is left to the sign-in
//! ladder.
//!
//! [`adopt_live_logins`] is the upkeep's step: it folds that reading into the record through the
//! accounts layer's one writer ([`accounts::adopt_login`]), whatever any tab's Autopilot says.
//! The fix is in the record, so every reader of [`accounts::signed_out_here`] agrees. The
//! sign-in ladder asks the same reading before it ranks a row ([`retain_not_live`]) and again
//! under the sign-in lock before it starts one, each from the one set of witnesses its pass
//! read ([`Pass`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
use aterm_json::Map;
use aterm_json::Value;

use super::accounts::{self, Auth, LoginEvidence, Roster, Row, State};
use super::tap;

/// The file in Claude Code's accounts store that keeps [`Baselines`].
#[cfg(unix)]
const BASELINES_FILE: &str = "tap-baselines.json";

/// The most of [`BASELINES_FILE`] that is read.
#[cfg(unix)]
const MAX_BASELINES: usize = 256 * 1024;

/// The most usage-tap files one reading takes ([`tap::readings_in`]).
const MOST_READINGS: usize = 512;

/// How many of the tap's refreshes ([`tap::TAP_REFRESH_S`]) a usage-tap file may miss and
/// still count as being written ([`being_written`]): a live tap moves its file's time at each,
/// and twelve cover a status line a loaded Mac ran late.
const QUIET_REFRESHES: i64 = 12;

/// The most conversations one pass reads the transcript of ([`Witnesses::read`]).
const MOST_CONVERSATIONS: usize = 8;

/// The most conversations one pass looks for the transcript of, to choose those it reads
/// ([`Witnesses::read`]): each costs one `stat` in every project directory
/// ([`transcripts_of`]) and no read.
const MOST_CANDIDATES: usize = 64;

/// The most of one transcript a pass reads, from its end ([`Conversation::read`]).
const CONVERSATION_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// The most transcript bytes one pass reads over all its conversations.
const PASS_BYTES: u64 = 16 * 1024 * 1024;

/// The most project directories searched for a conversation's transcript
/// ([`transcripts_of`]).
const MOST_PROJECTS: usize = 4096;

/// The whole content of the row Claude Code writes when `/login` succeeds.
const LOGIN_MADE: &str = "<local-command-stdout>Login successful</local-command-stdout>";

/// How the content of the row Claude Code writes for the `/login` command begins.
const LOGIN_COMMAND: &str = "<command-name>/login</command-name>";

/// Whether a usage-tap file last written at `written_at` is still being written at `now`.
fn being_written(written_at: i64, now: i64) -> bool {
    now.saturating_sub(written_at) <= QUIET_REFRESHES * i64::from(tap::TAP_REFRESH_S)
}

/// What one usage-tap file said when this module first looked at it after a sign-out: the floor
/// its API time must grow past to show its process was served since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Baseline {
    /// The file's `api_ms` then.
    pub(super) api_ms: u64,
    /// When it was taken (Unix seconds).
    pub(super) taken_at: i64,
    /// The conversation the file named then. A file that names another one now is another
    /// process's or another conversation's count, and its baseline is taken again.
    pub(super) session: Option<String>,
}

impl Baseline {
    /// THE ONE RULE OF A BASELINE THAT STANDS for `reading` against a sign-out recorded at
    /// `struck`, at `now`: it was taken after that sign-out and not after now, of the
    /// conversation the file still names, and the file's API time has not fallen under it. The
    /// reading asks it of each row's own sign-out ([`Witnesses::live_login_here`]) and the
    /// keeper of the newest one on record ([`Baselines::retaken`]); a baseline that fails it
    /// shows nothing and is taken again.
    pub(super) fn stands(&self, reading: &tap::Reading, struck: i64, now: i64) -> bool {
        self.taken_at > struck
            && self.taken_at <= now
            && self.session == reading.session
            && reading.api_ms >= self.api_ms
    }
}

/// The baselines of every usage-tap file, by the file's stem (`<sid>-<pid>`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Baselines(pub(super) BTreeMap<String, Baseline>);

impl Baselines {
    #[cfg(unix)]
    fn to_json(&self) -> String {
        let mut files = Map::new();
        for (stem, base) in &self.0 {
            let mut o = Map::new();
            o.insert("api_ms".into(), Value::from(base.api_ms));
            o.insert("taken_at".into(), Value::from(base.taken_at));
            o.insert("session".into(), Value::from(base.session.clone()));
            files.insert(stem.clone(), Value::Object(o));
        }
        let mut o = Map::new();
        o.insert("schema".into(), Value::from(1_u64));
        o.insert("files".into(), Value::Object(files));
        Value::Object(o).to_string()
    }

    #[cfg(unix)]
    fn from_json(bytes: &[u8]) -> Option<Self> {
        let value: Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("schema")?.as_u64()? != 1 {
            return None;
        }
        let files = value
            .get("files")?
            .as_object()?
            .iter()
            .map(|(stem, base)| {
                let session = match base.get("session")? {
                    Value::Null => None,
                    Value::String(session) => Some(session.clone()),
                    _ => return None,
                };
                Some((
                    stem.clone(),
                    Baseline {
                        api_ms: base.get("api_ms")?.as_u64()?,
                        taken_at: base.get("taken_at")?.as_i64()?,
                        session,
                    },
                ))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        Some(Self(files))
    }

    /// The baselines kept in the store directory `dir`; none where the file is missing or does
    /// not read as one, which only delays evidence: every baseline is then taken again.
    #[cfg(unix)]
    pub(super) fn kept(dir: &Path) -> Self {
        aterm_uds::private_file::read_private(dir, BASELINES_FILE, MAX_BASELINES)
            .ok()
            .and_then(|bytes| Self::from_json(&bytes))
            .unwrap_or_default()
    }

    /// Off Unix no strict private file is kept, so no baseline is and no reading counts as
    /// evidence.
    #[cfg(not(unix))]
    pub(super) fn kept(_dir: &Path) -> Self {
        Self::default()
    }

    /// Keeps these in `dir`, or removes the file where there are none. The caller holds the
    /// accounts lock.
    #[cfg(unix)]
    fn keep(&self, dir: &Path) -> std::io::Result<()> {
        if self.0.is_empty() {
            aterm_uds::private_file::remove_private(dir, BASELINES_FILE).map(|_| ())
        } else {
            aterm_uds::private_file::write_private(dir, BASELINES_FILE, self.to_json().as_bytes())
        }
    }

    #[cfg(not(unix))]
    fn keep(&self, _dir: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// These after a look at `readings` at `now`, where the newest sign-out on record is
    /// `newest_strike`: a file that is still being written ([`being_written`]) keeps its
    /// baseline while that stands against that sign-out ([`Baseline::stands`]), and has it
    /// taken now, at what it says now, where it does not. A baseline that is kept is not moved
    /// up, so growth past it stays evidence until the row is lifted; one taken again shows
    /// nothing until the file grows once more. A file that is gone, or that its process
    /// stopped writing (it ended, or its tab moved on), keeps none: when it is written again
    /// its baseline is taken then.
    pub(super) fn retaken(&self, readings: &[tap::Kept], newest_strike: i64, now: i64) -> Self {
        Self(
            readings
                .iter()
                .filter(|kept| being_written(kept.written_at, now))
                .map(|kept| {
                    let reading = &kept.reading;
                    let stood = self
                        .0
                        .get(&kept.stem)
                        .filter(|base| base.stands(reading, newest_strike, now));
                    let base = stood.cloned().unwrap_or_else(|| Baseline {
                        api_ms: reading.api_ms,
                        taken_at: now,
                        session: reading.session.clone(),
                    });
                    (kept.stem.clone(), base)
                })
                .collect(),
        )
    }
}

/// When `row`'s login was recorded signed out, where the word on record for its login reads
/// signed out now ([`accounts::Account::current_auth`], the record half of
/// [`accounts::signed_out_here`]) and carries its time.
#[must_use]
pub(super) fn struck_at(row: &Row, state: &State) -> Option<i64> {
    let account = state.accounts.get(&row.label)?;
    (account.current_auth(row) == Auth::SignedOut)
        .then_some(account.auth_at)
        .flatten()
}

/// Every row of `roster` signed out on record, each with when ([`struck_at`]).
fn struck_rows(roster: &Roster, state: &State) -> Vec<(Row, i64)> {
    roster
        .rows
        .iter()
        .filter_map(|row| Some((row.clone(), struck_at(row, state)?)))
        .collect()
}

/// The login directory `row` names, where it names one; `None` for the default login's row.
fn login_dir(row: &Row) -> Option<&str> {
    row.dir.as_deref().filter(|dir| !dir.is_empty())
}

/// WHAT ONE CONVERSATION'S TRANSCRIPT SHOWS SINCE ITS LAST LOGIN WALL, of its main chain alone
/// (no subagent's row): each `/login` that succeeded and each request that was served. A row
/// Claude Code's lost-login answer follows shows nothing: the login it was of is gone again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Conversation {
    /// When each `/login` that succeeded wrote its row.
    logins: Vec<i64>,
    /// Each served request: when the prompt or tool result that began it was written, and when
    /// the first row of the model's answer to it was (a response made after that row was
    /// written, never a later row of the response then in flight).
    served: Vec<(i64, i64)>,
}

/// What kind of row begins a request ([`Conversation::begins_request`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Begins {
    /// A prompt: a person's, or one Claude Code was handed.
    Prompt,
    /// A tool's result, which follows the response that asked for the tool.
    ToolResult,
}

/// Where a fold of a transcript's rows stands ([`Conversation::fold`]).
#[derive(Default)]
struct Fold {
    /// The newest row of the chain the request in hand began: a row that names it as its
    /// parent continues that chain. `None` before any request, or where its row had no id.
    tip: Option<String>,
    /// When the prompt or tool result that began the request in hand was written.
    asked: Option<i64>,
    /// The request in hand already has its first answer.
    answered: bool,
    /// The last local command's row was `/login`'s own.
    login_command: bool,
    /// A main-chain assistant row was folded: [`Self::response`] is of it.
    responded: bool,
    /// The API response the last main-chain assistant row is a row of (its `message.id`: one
    /// response writes a row for each block of its content, all under one id), `None` where
    /// that row carried none or no such row was folded.
    response: Option<String>,
    /// [`Self::response`] as it stood when the request in hand began: the response that was
    /// in flight then, if one was. A row of it is no answer to the request.
    in_flight: Option<String>,
    /// The rows folded are the end of a transcript whose earlier rows were not read.
    cut: bool,
}

impl Conversation {
    /// The text of a local command's row: a `local_command` system row's content, or the
    /// plain-string content of a user row that is no prompt (a prompt a person typed or
    /// pasted carries its origin; a tool result's content is a list).
    fn command_text(v: &Value) -> Option<&str> {
        match v.get("type").and_then(Value::as_str) {
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("local_command") => {
                v.get("content").and_then(Value::as_str)
            }
            Some("user") if v.get("origin").is_none() && v.get("turnOrigin").is_none() => v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .filter(|text| {
                    let text = text.trim_start();
                    text.starts_with("<local-command-")
                        || text.starts_with("<command-name>")
                        || text.starts_with("<command-message>")
                }),
            _ => None,
        }
    }

    /// What kind of request the row `v` begins, where it begins one: a user row that is a
    /// prompt (text, or parts with text or an attachment) or carries a tool's result. A local
    /// command's row, a row Claude Code wrote for itself (`isMeta`) and a compaction's summary
    /// begin none.
    fn begins_request(v: &Value) -> Option<Begins> {
        if v.get("type").and_then(Value::as_str) != Some("user")
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
            || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            || Self::command_text(v).is_some()
        {
            return None;
        }
        match v.get("message").and_then(|m| m.get("content")) {
            Some(Value::String(_)) => Some(Begins::Prompt),
            Some(Value::Array(parts)) => {
                let holds = |kinds: &[&str]| {
                    parts.iter().any(|part| {
                        part.get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| kinds.contains(&kind))
                    })
                };
                if holds(&["tool_result"]) {
                    Some(Begins::ToolResult)
                } else {
                    holds(&["text", "image", "document"]).then_some(Begins::Prompt)
                }
            }
            _ => None,
        }
    }

    /// The API response the assistant row `v` is a row of: its `message.id`, where it carries
    /// one.
    fn response_of(v: &Value) -> Option<&str> {
        v.get("message")
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    }

    /// Whether the row `v` is the model's answer: an assistant row of a model, not an API
    /// error Claude Code wrote in its place.
    fn answers(v: &Value) -> bool {
        v.get("type").and_then(Value::as_str) == Some("assistant")
            && v.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true)
            && v.get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .is_some_and(super::upgrade::is_model_id)
    }

    /// Folds one transcript line into this, in the order the transcript holds its rows.
    ///
    /// A `/login` THAT SUCCEEDED is a top-level local command's row whose whole content is the
    /// command's `Login successful` output, straight after the row of the `/login` command
    /// itself. A subagent's row, a tool's result, a prompt a person typed or pasted, and any
    /// row that only quotes those words are none.
    ///
    /// A SERVED REQUEST is a prompt or tool result ([`Self::begins_request`]) and the first
    /// answer ([`Self::answers`]) of ANOTHER API RESPONSE than the one in flight when that row
    /// was written, on the chain of rows that leads from it, each naming the row before it as
    /// its parent, dated no earlier than it. An answer that names another parent (a request
    /// begun before this one that ends after it) is of no request here.
    ///
    /// One API response writes a row for each block of its content, all under one
    /// `message.id`, and Claude Code runs a tool while the response that asked for it still
    /// streams: the tool's result is then written between two rows of that one response, and
    /// the row chained after it was requested before it. So a row of the response the last
    /// assistant row before the request's own row belongs to ([`Fold::in_flight`]) goes on
    /// with the chain and answers nothing, and an answer that carries no response id answers
    /// nothing either, since nothing tells it from the response before. A tool's result with
    /// no assistant row read before it begins no request (a tool's result always follows the
    /// response that asked for it, so the read's cut hid that response), and neither does a
    /// prompt with none in a transcript read from a cut ([`Fold::cut`]).
    fn fold(&mut self, fold: &mut Fold, line: &str) {
        let Ok(v) = super::transcript::conversation(line) else {
            return;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let at = super::upgrade::row_time(&v).and_then(|at| i64::try_from(at).ok());
        if super::upgrade::is_auth_wall_row(&v) {
            self.logins.clear();
            self.served.clear();
            (fold.asked, fold.answered, fold.login_command) = (None, false, false);
        }
        match Self::command_text(&v).map(str::trim) {
            Some(text) if text.starts_with(LOGIN_COMMAND) => fold.login_command = true,
            Some(text) => {
                if text == LOGIN_MADE
                    && fold.login_command
                    && let Some(at) = at
                {
                    self.logins.push(at);
                }
                fold.login_command = false;
            }
            None if matches!(
                v.get("type").and_then(Value::as_str),
                Some("user" | "assistant")
            ) =>
            {
                fold.login_command = false;
            }
            None => {}
        }
        let id = v.get("uuid").and_then(Value::as_str);
        if let Some(begins) = Self::begins_request(&v) {
            // The response before this row is known where an assistant row was read before
            // it, or, for a prompt, where the transcript was read from its first row (a
            // conversation's first prompt follows none).
            let known = fold.responded || (begins == Begins::Prompt && !fold.cut);
            fold.tip = id.filter(|_| known).map(str::to_owned);
            (fold.asked, fold.answered) = (at.filter(|_| known), false);
            fold.in_flight.clone_from(&fold.response);
            return;
        }
        let response = (v.get("type").and_then(Value::as_str) == Some("assistant"))
            .then(|| Self::response_of(&v).map(str::to_owned));
        let parent = v.get("parentUuid").and_then(Value::as_str);
        if let (Some(id), Some(parent)) = (id, parent)
            && fold.tip.as_deref() == Some(parent)
        {
            fold.tip = Some(id.to_owned());
            let another = response
                .as_ref()
                .is_some_and(|response| response.is_some() && *response != fold.in_flight);
            if !fold.answered
                && another
                && Self::answers(&v)
                && let (Some(asked), Some(at)) = (fold.asked, at)
                && at >= asked
            {
                self.served.push((asked, at));
                fold.answered = true;
            }
        }
        if let Some(response) = response {
            (fold.responded, fold.response) = (true, response);
        }
    }

    /// What the whole transcript rows `rows` show, in the order given; `cut` where they are
    /// the end of a transcript whose earlier rows were not read.
    fn of_rows<'a>(rows: impl Iterator<Item = &'a [u8]>, cut: bool) -> Self {
        let mut conversation = Self::default();
        let mut fold = Fold {
            cut,
            ..Fold::default()
        };
        for row in rows {
            if let Ok(line) = std::str::from_utf8(row) {
                conversation.fold(&mut fold, line);
            }
        }
        conversation
    }

    /// THE END OF THE TRANSCRIPT AT `path`, read once: its last [`CONVERSATION_TAIL_BYTES`] at
    /// most, and no more than `budget` holds (what is read is taken off it), in whole rows. A
    /// row the read began inside and a last row with no newline (one still being written) are
    /// left out. A transcript not written since `since` holds no row dated after it and is not
    /// read. `None` where it cannot be read; what a cut left out only delays evidence.
    fn read(path: &Path, since: i64, budget: &mut u64) -> Option<Self> {
        let (mut file, meta) = super::footer::open_regular_with_meta(path)?;
        let written_after = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|at| i64::try_from(at.as_secs()).ok())
            .is_none_or(|at| at > since);
        if !written_after {
            return Some(Self::default());
        }
        let take = meta.len().min(CONVERSATION_TAIL_BYTES).min(*budget);
        let start = meta.len() - take;
        // One byte early, so a row that begins exactly at `start` is known to be whole: what
        // comes before the first newline read is the end of the row before it.
        let from = start.saturating_sub(1);
        file.seek(std::io::SeekFrom::Start(from)).ok()?;
        let mut bytes = Vec::with_capacity(usize::try_from(take).ok()?.saturating_add(1));
        file.take(meta.len() - from).read_to_end(&mut bytes).ok()?;
        *budget -= take;
        let mut rows: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
        // The piece after the last newline is a row still being written, or nothing.
        rows.pop();
        let skip = usize::from(from > 0);
        Some(Self::of_rows(rows.into_iter().skip(skip), from > 0))
    }
}

/// Where the transcript of each conversation of `sessions` is under the Claude home
/// `claude_dir`, and when it was last written (Unix seconds; `i64::MAX` where the file says no
/// time after the epoch, which [`Conversation::read`] takes as written since any time):
/// `projects/<project>/<conversation>.jsonl`, a regular file, the most recently written where
/// more than one project holds it. No directory is listed below `projects`, and at most
/// [`MOST_PROJECTS`] of its entries are tried; no file is opened.
fn transcripts_of(
    claude_dir: &Path,
    sessions: &BTreeSet<String>,
) -> BTreeMap<String, (i64, PathBuf)> {
    let Ok(projects) = std::fs::read_dir(claude_dir.join("projects")) else {
        return BTreeMap::new();
    };
    let mut found: BTreeMap<String, (Option<std::time::SystemTime>, PathBuf)> = BTreeMap::new();
    for project in projects.flatten().take(MOST_PROJECTS) {
        if !project.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        for session in sessions {
            let path = project.path().join(format!("{session}.jsonl"));
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.file_type().is_file() {
                continue;
            }
            let written = meta.modified().ok();
            if found
                .get(session)
                .is_none_or(|(newest, _)| written > *newest)
            {
                found.insert(session.clone(), (written, path));
            }
        }
    }
    found
        .into_iter()
        .map(|(session, (written, path))| {
            let written = written
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|at| i64::try_from(at.as_secs()).ok())
                .unwrap_or(i64::MAX);
            (session, (written, path))
        })
        .collect()
}

/// The credential-free files one pass's readings are made from, read once ([`Pass`]).
pub(super) struct Witnesses {
    /// The person's home, whose `.claude.json` is the default login's profile.
    user_home: PathBuf,
    /// Every usage-tap file that reads.
    readings: Vec<tap::Kept>,
    /// The baselines on disk when this was read.
    baselines: Baselines,
    /// What the transcript of each conversation that could show a row live says, by
    /// conversation.
    conversations: BTreeMap<String, Conversation>,
}

impl Witnesses {
    /// Reads the usage-tap files under the harness state `state_dir`, the baselines `store`
    /// keeps, and the transcripts, under the default home of `home`, of the conversations
    /// that could show one of `struck` (each row signed out on record, with when) live: those
    /// a file of the row's own stored login names under a baseline that stands against the
    /// row's sign-out, for a row whose login place proves itself the row's
    /// ([`Self::names_row`]). No other transcript is opened.
    ///
    /// WHICH ARE READ IS CHOSEN BY THE TRANSCRIPT, NOT THE USAGE-TAP FILE: every live tab's tap
    /// moves its file's time at each refresh, idle or not, so that time says nothing of which
    /// tab a person just used. The transcripts of the [`MOST_CANDIDATES`] most recently
    /// written such files' conversations are looked for ([`transcripts_of`], a `stat` each);
    /// one not written after the time its rows would have to be dated after holds no evidence
    /// and takes no place; and of the rest the [`MOST_CONVERSATIONS`] most recently written
    /// are read, each from its end ([`Conversation::read`]) under [`PASS_BYTES`] in all. So
    /// the tab a person typed `/login` in is read however many idle tabs share its login.
    pub(super) fn read(
        home: &Path,
        state_dir: &Path,
        store: &accounts::Store,
        struck: &[(Row, i64)],
        now: i64,
    ) -> Self {
        let mut witnesses = Self {
            user_home: home.to_path_buf(),
            readings: tap::readings_in(&tap::rl_dir_in(state_dir), MOST_READINGS),
            baselines: Baselines::kept(&store.dir),
            conversations: BTreeMap::new(),
        };
        // Each wanted conversation, with the earliest time a row of it could count from and
        // when the file that names it was last written.
        let mut wanted: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
        for (row, at) in struck {
            if !witnesses.names_row(row) {
                continue;
            }
            for kept in &witnesses.readings {
                let Some(base) = witnesses.standing(kept, row, *at, now) else {
                    continue;
                };
                let Some(session) = kept
                    .reading
                    .session
                    .as_deref()
                    .filter(|session| super::upgrade::is_session_id(session))
                else {
                    continue;
                };
                let floor = base.taken_at.max(*at);
                let entry = wanted.entry(session).or_insert((floor, kept.written_at));
                *entry = (entry.0.min(floor), entry.1.max(kept.written_at));
            }
        }
        let mut candidates: Vec<(&str, (i64, i64))> = wanted.into_iter().collect();
        candidates.sort_by_key(|(_, (_, written_at))| std::cmp::Reverse(*written_at));
        candidates.truncate(MOST_CANDIDATES);
        let sessions: BTreeSet<String> = candidates
            .iter()
            .map(|(session, _)| (*session).to_owned())
            .collect();
        let found = transcripts_of(&home.join(".claude"), &sessions);
        // Each transcript written after its floor, the most recently written first.
        let mut order: Vec<(i64, i64, &Path, &str)> = candidates
            .iter()
            .filter_map(|(session, (floor, _))| {
                let (written, path) = found.get(*session)?;
                (*written > *floor).then_some((*written, *floor, path.as_path(), *session))
            })
            .collect();
        order.sort_by_key(|(written, ..)| std::cmp::Reverse(*written));
        order.truncate(MOST_CONVERSATIONS);
        let mut budget = PASS_BYTES;
        let mut conversations = BTreeMap::new();
        for (_, floor, path, session) in order {
            if let Some(read) = Conversation::read(path, floor, &mut budget) {
                conversations.insert(session.to_owned(), read);
            }
        }
        witnesses.conversations = conversations;
        witnesses
    }

    /// Whether `reading` is of a process on `row`'s own stored login: its tap's environment
    /// gave it no credential, profile or route variable ([`tap::Route::Stored`]; a token
    /// route's reading is its token's and an outside route's is nobody's here), it runs in the
    /// default Claude home, and its login place is the row's (the default login for a row with
    /// no directory, else that directory by its text, as a launch names it).
    fn of_row(reading: &tap::Reading, row: &Row) -> bool {
        let named = |text: &Option<String>| text.clone().filter(|text| !text.is_empty());
        reading.route == Some(tap::Route::Stored)
            && named(&reading.home).is_none()
            && named(&reading.login) == named(&row.dir)
    }

    /// The baseline of the file `kept` where that file is of `row`'s own stored login
    /// ([`Self::of_row`]) and its baseline stands against the row's sign-out at `struck`
    /// ([`Baseline::stands`]): this module's own earlier look at the same file, after the
    /// sign-out, holding the same conversation.
    fn standing(&self, kept: &tap::Kept, row: &Row, struck: i64, now: i64) -> Option<&Baseline> {
        if !Self::of_row(&kept.reading, row) {
            return None;
        }
        self.baselines
            .0
            .get(&kept.stem)
            .filter(|base| base.stands(&kept.reading, struck, now))
    }

    /// WHETHER `row`'S LOGIN PLACE PROVES ITSELF THE ROW'S OWN, by a file that holds no
    /// credential: for the default login's row, the default home's profile names the row's
    /// address, a known match (a `/login` in a tab of the default home rewrites that profile).
    /// A profile that names another account, cannot be read, or names none answers no, and so
    /// does a row that names no address.
    ///
    /// A row with a login directory of its own is proved by nothing here. A process on such a
    /// row keeps its profile in the default home and only its keychain item under the
    /// directory (`docs/DESIGN-account-discovery-2026-11-09.md`), so a `/login` in its tab
    /// writes no file that says which account the directory's place now holds: the directory's
    /// own profile is written only by a sign-in staged there, and goes on naming the row after
    /// a person signs that place in as someone else. Such a row answers no
    /// ([`Self::unproved`]) and is left to the sign-in ladder.
    pub(super) fn names_row(&self, row: &Row) -> bool {
        login_dir(row).is_none()
            && row.expected_identity_email().is_some_and(|email| {
                super::accounts_sync::default_profile_names(&self.user_home, email)
            })
    }

    /// Whether `row` is one with a login directory of its own ([`Self::names_row`] proves
    /// none such) whose tab looks served since its sign-out at `struck`: a file of its own
    /// stored login, still being written, whose API time grew past a baseline that stands.
    /// The adoption step declines it and says why ([`Refusal::Unproved`]).
    fn unproved(&self, row: &Row, struck: i64, now: i64) -> bool {
        login_dir(row).is_some()
            && self.readings.iter().any(|kept| {
                being_written(kept.written_at, now)
                    && self
                        .standing(kept, row, struck, now)
                        .is_some_and(|base| kept.reading.api_ms > base.api_ms)
            })
    }

    /// THE ONE LIVENESS READING: the earliest evidence that `row`'s own login was signed in or
    /// served after `struck` (when its sign-out was recorded) and not after `now`; `None`
    /// where no file shows it or its login place does not prove itself the row's
    /// ([`Self::names_row`]).
    ///
    /// Both kinds rest on a file of the row's own stored login whose baseline stands
    /// ([`Self::standing`]) and on the transcript of the conversation that file names
    /// ([`Conversation`]), and count only what is dated after both the sign-out and the
    /// baseline, in a file written at or after the evidence's own time (a file its process
    /// stopped writing before then says nothing of who holds the conversation now).
    ///
    /// Signed in: a `/login` that succeeded then (a row from before this module first saw the
    /// process hold the conversation may be of another login the conversation ran on).
    ///
    /// Served: a request that BEGAN then (its prompt or tool result is dated then) and was
    /// answered by `now` by a response that was not already in flight when that row was
    /// written ([`Conversation::fold`]), alongside API time grown past the baseline with a
    /// rate-limit figure first reported after the sign-out. A request authorized before the
    /// sign-out that ends after it grows the API time too, and is none, whether or not a
    /// tool's result was written while its response streamed.
    #[must_use]
    pub(super) fn live_login_here(
        &self,
        row: &Row,
        struck: i64,
        now: i64,
    ) -> Option<LoginEvidence> {
        if !self.names_row(row) {
            return None;
        }
        let mut found: Option<LoginEvidence> = None;
        let mut take = |evidence: LoginEvidence| {
            if found.is_none_or(|held| evidence.at() < held.at()) {
                found = Some(evidence);
            }
        };
        for kept in &self.readings {
            let Some(base) = self.standing(kept, row, struck, now) else {
                continue;
            };
            let reading = &kept.reading;
            let Some(conversation) = reading
                .session
                .as_deref()
                .and_then(|session| self.conversations.get(session))
            else {
                continue;
            };
            let floor = base.taken_at.max(struck);
            let shown = |at: i64| at <= now && kept.written_at >= at;
            for at in &conversation.logins {
                if floor < *at && shown(*at) {
                    take(LoginEvidence::SignedIn(*at));
                }
            }
            let grew = reading.api_ms > base.api_ms
                && reading.has_figures()
                && struck < reading.at
                && reading.at <= now;
            if grew
                && let Some((_, answered)) = conversation
                    .served
                    .iter()
                    .find(|(asked, answered)| floor < *asked && shown(*answered))
            {
                take(LoginEvidence::Served(*answered));
            }
        }
        found
    }

    /// [`Self::live_login_here`] for `row` as `state` records it: evidence newer than its
    /// sign-out on record, or `None` where it is not signed out on record.
    #[must_use]
    pub(super) fn newer_than_sign_out(
        &self,
        row: &Row,
        state: &State,
        now: i64,
    ) -> Option<LoginEvidence> {
        self.live_login_here(row, struck_at(row, state)?, now)
    }
}

/// ONE PASS'S WITNESSES, READ ONCE AND HANDED TO EVERY ASKER: the upkeep's adoption step, each
/// read of the ladder's candidates and the last ask under the sign-in lock all read the files
/// through this, so a pass opens each usage-tap file and each transcript at most once
/// however many rows it asks about. It also holds which rows' lifts this pass's step (or, for
/// a pass with no step of its own, an earlier one) was refused ([`Refusal`]): evidence hides
/// no such row from the ladder.
#[derive(Default)]
pub(super) struct Pass {
    witnesses: OnceLock<Witnesses>,
    refused: Mutex<BTreeSet<String>>,
}

impl Pass {
    /// A pass that knows the rows `refused` an earlier step could not lift.
    pub(super) fn knowing(refused: impl IntoIterator<Item = String>) -> Self {
        Self {
            witnesses: OnceLock::new(),
            refused: Mutex::new(refused.into_iter().collect()),
        }
    }

    /// The pass's witnesses, read by `read` where this is the first ask.
    fn witnesses(&self, read: impl FnOnce() -> Witnesses) -> &Witnesses {
        self.witnesses.get_or_init(read)
    }

    /// Whether a step was refused `label`'s lift.
    pub(super) fn refused(&self, label: &str) -> bool {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(label)
    }

    /// Notes that a step was refused `label`'s lift.
    pub(super) fn refuse(&self, label: &str) {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(label.to_owned());
    }

    /// THE READING FOR ONE ROW OF THE RECORDS A CALLER HOLDS: the evidence that `label`'s
    /// login is live after its recorded sign-out ([`Witnesses::newer_than_sign_out`]), from
    /// this pass's witnesses.
    pub(super) fn live(
        &self,
        (home, state_dir): (&Path, &Path),
        store: &accounts::Store,
        (roster, state): (&Roster, &State),
        label: &str,
        now: i64,
    ) -> Option<LoginEvidence> {
        let row = roster.row(label)?;
        struck_at(row, state)?;
        self.witnesses(|| Witnesses::read(home, state_dir, store, &struck_rows(roster, state), now))
            .newer_than_sign_out(row, state, now)
    }
}

/// EVERY ROUTE THAT COULD START A SIGN-IN ASKS FIRST: drops from the ranked rows `tiered` each
/// one whose own login the files show live after its recorded sign-out
/// ([`Witnesses::newer_than_sign_out`]), unless a step was refused its lift ([`Pass::refused`]:
/// a row whose record cannot be lifted stays the ladder's to sign in). It reads the pass's
/// witnesses and writes nothing: the record is lifted by the upkeep's step
/// ([`adopt_live_logins`]), and a row that is not signed out on record (one due by its forecast
/// end) is ranked as it was. `roster` and `state` are the records the ranking was made from.
pub(super) fn retain_not_live(
    tiered: &mut Vec<(u8, Row)>,
    paths: (&Path, &Path),
    store: &accounts::Store,
    (roster, state): (&Roster, &State),
    now: i64,
    pass: &Pass,
) {
    tiered.retain(|(_, row)| {
        pass.refused(&row.label)
            || pass
                .live(paths, store, (roster, state), &row.label, now)
                .is_none()
    });
}

/// A login [`adopt_live_logins`] adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Adopted {
    /// The row.
    pub(super) label: String,
    /// What showed its login live.
    pub(super) evidence: LoginEvidence,
}

/// Why the adoption step left a row signed out on record though a file shows its login live,
/// for a reason no later pass ends by itself. Its words name no address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The row has a login directory of its own, and no file without a credential shows which
    /// account that place holds ([`Witnesses::names_row`]).
    Unproved,
    /// The roster's row changed between the reading and the write.
    RowChanged,
    /// Under the writer's lock the default login's profile no longer names the row.
    Profile,
    /// The one writer refused the evidence ([`accounts::adopt_login`]).
    Record(accounts::NotAdopted),
    /// The record could not be written.
    Store,
}

impl Refusal {
    /// Its words in the log.
    pub(super) fn words(self) -> String {
        match self {
            Self::Unproved => "it has a login directory of its own, and no file without a \
                               credential shows which account that place holds now"
                .to_owned(),
            Self::RowChanged => "its roster row changed while it was read".to_owned(),
            Self::Profile => "the default login's profile no longer names it".to_owned(),
            Self::Record(why) => why.words(),
            Self::Store => "its record could not be written".to_owned(),
        }
    }
}

/// How the adoption step left one row a file shows live ([`Step::lifts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lift {
    /// Its sign-out is lifted on this evidence.
    Landed(LoginEvidence),
    /// Its record no longer read signed out under the writer's lock: another process's step
    /// lifted it first.
    Already,
    /// The accounts lock stayed held: the next pass tries again.
    Busy,
    /// Nothing was lifted, for a reason a later pass does not end by itself.
    Refused(Refusal),
}

/// What one adoption step did ([`adopt_live_logins`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Step {
    /// Each row a file shows live, by label, and how the step left it.
    pub(super) lifts: Vec<(String, Lift)>,
    /// The baselines could not be kept, so no later pass can show a tab served until they can.
    pub(super) baselines_unkept: bool,
    /// The account records could not be read, so no row was asked about: the step says
    /// nothing of any, and what an earlier step said stands.
    pub(super) unread: bool,
}

impl Step {
    /// The logins the step adopted.
    pub(super) fn adopted(&self) -> Vec<Adopted> {
        self.lifts
            .iter()
            .filter_map(|(label, lift)| match lift {
                Lift::Landed(evidence) => Some(Adopted {
                    label: label.clone(),
                    evidence: *evidence,
                }),
                _ => None,
            })
            .collect()
    }

    /// How the step left `label`, where a file shows its login live.
    pub(super) fn lift_of(&self, label: &str) -> Option<Lift> {
        self.lifts
            .iter()
            .find(|(of, _)| of == label)
            .map(|(_, lift)| *lift)
    }
}

/// THE UPKEEP'S ADOPTION STEP: for each Claude Code row signed out on record, read whether its
/// own login was signed in or served since ([`Witnesses::live_login_here`], from `pass`'s
/// witnesses) and, where it was, lift the sign-out through the one writer
/// ([`accounts::adopt_login`]) in one transaction of the store under `state_dir`, returning how
/// it left each such row ([`Step`]) and saying each lift in `log`. It needs no tab supervisor
/// and reads no tab's Autopilot. It also keeps the baselines a later pass's evidence is
/// measured from ([`Baselines::retaken`], made under the accounts lock from what the file
/// holds then), and removes them once no sign-out stands.
///
/// Under the writer's lock the row is read again, and so is the profile that proves whose login
/// it is, so a roster edit or a login made between the reading and the write decides: evidence
/// read of a row that changed since adopts nothing. Two processes that fold the same evidence
/// agree, since the writer stamps the word at the evidence's own time and lifts only a word
/// that still reads signed out. A row with a login directory of its own is never lifted
/// ([`Refusal::Unproved`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLoginAdoption",
        action = "Adopt",
        project = "aterm_agent::harness::login_adoption::tests::adoption_transition"
    )
)]
pub(super) fn adopt_live_logins(
    home: &Path,
    state_dir: &Path,
    roster_path: &Path,
    now: i64,
    pass: &Pass,
    log: &dyn Fn(&str),
) -> Step {
    let mut step = Step::default();
    let store = accounts::Store::at_for(state_dir, accounts::Cli::Claude, home);
    let Ok(records) = store.read_account_records(Some(roster_path), now) else {
        step.unread = true;
        return step;
    };
    let struck = struck_rows(&records.roster, &records.state);
    let Some(newest) = struck.iter().map(|(_, at)| *at).max() else {
        // No sign-out stands, so no baseline is of use: a later sign-out takes its own.
        if !Baselines::kept(&store.dir).0.is_empty()
            && let Ok(Some(_held)) = store.acquire_bounded()
        {
            step.baselines_unkept = Baselines::default().keep(&store.dir).is_err();
        }
        return step;
    };
    let witnesses = pass.witnesses(|| Witnesses::read(home, state_dir, &store, &struck, now));
    let found: Vec<(Row, LoginEvidence)> = struck
        .iter()
        .filter_map(|(row, at)| Some((row.clone(), witnesses.live_login_here(row, *at, now)?)))
        .collect();
    step.lifts.extend(
        struck
            .iter()
            .filter(|(row, at)| witnesses.unproved(row, *at, now))
            .map(|(row, _)| (row.label.clone(), Lift::Refused(Refusal::Unproved))),
    );
    let retaken = |kept: &Baselines| kept.retaken(&witnesses.readings, newest, now);
    let kept = Baselines::kept(&store.dir);
    if found.is_empty() && retaken(&kept) == kept {
        return step;
    }
    let all = |lift: Lift| found.iter().map(move |(row, _)| (row.label.clone(), lift));
    let held = match store.acquire_bounded() {
        Ok(Some(held)) => held,
        Ok(None) => {
            step.lifts.extend(all(Lift::Busy));
            return step;
        }
        Err(_) => {
            step.lifts.extend(all(Lift::Refused(Refusal::Store)));
            return step;
        }
    };
    // Under the lock, from what the file holds now: another process's step may have kept
    // its own since the look above.
    let kept = Baselines::kept(&store.dir);
    let next = retaken(&kept);
    if next != kept {
        step.baselines_unkept = next.keep(&store.dir).is_err();
    }
    if found.is_empty() {
        return step;
    }
    let mut lifts = Vec::new();
    let done = store.transact_pool_logins_held(
        &held,
        roster_path,
        now,
        &mut None,
        |raw, state, logins| {
            let roster = accounts::bound(raw, logins);
            let mut events = Vec::new();
            for (read_row, evidence) in &found {
                // The row as this transaction holds it, and whose login it is, read again
                // under the writer's lock.
                let lift = match roster.row(&read_row.label).filter(|row| *row == read_row) {
                    None => Lift::Refused(Refusal::RowChanged),
                    Some(row) if !witnesses.names_row(row) => Lift::Refused(Refusal::Profile),
                    Some(row) => match accounts::adopt_login(state, logins, row, *evidence, now) {
                        Ok(lifted) => {
                            events.extend(lifted);
                            Lift::Landed(*evidence)
                        }
                        Err(accounts::NotAdopted::NotSignedOut) => Lift::Already,
                        Err(why) => Lift::Refused(Refusal::Record(why)),
                    },
                };
                lifts.push((read_row.label.clone(), lift));
            }
            Ok(events)
        },
    );
    drop(held);
    if done.is_err() {
        step.lifts.extend(all(Lift::Refused(Refusal::Store)));
        return step;
    }
    step.lifts.extend(lifts);
    for lift in step.adopted() {
        log(&format!(
            "account {}: signed in again ({}, at {}): its recorded sign-out is lifted",
            lift.label,
            lift.evidence.words(),
            lift.evidence.at()
        ));
    }
    step
}

/// The environment the adoption model's fixture chooses.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginAdoption",
        action = "NotSignedOut",
        reason = "Native fixture selection of a row that reads signed in on record."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginAdoption",
        action = "LaterEvidence",
        reason = "Native fixture selection of evidence dated before, after and past now."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginAdoption",
        action = "AnotherOwner",
        reason = "Native fixture selection of a profile naming the row, another account or none, and of a row on a login directory of its own."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginAdoption",
        action = "InFlight",
        reason = "Native fixture selection of a request begun before the sign-out that is answered after it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginAdoption",
        action = "SignedIn",
        reason = "Native fixture selection of served evidence or a sign-in made in a tab."
    )
)]
#[doc(hidden)]
pub fn account_login_adoption_environment() {}

#[cfg(test)]
#[path = "login_adoption_tests.rs"]
pub(super) mod tests;
