// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TASK RECORD (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.2): what a
//! session started with a task — a WORKER — is doing about it, as one small
//! record a manager reads, waits on and is pushed.
//!
//! A session has a record only once something marks it; no record is no task.
//! Every writer lands through the in-process [`mark_session_task`]: `task
//! mark` — the window harness marks a worker `done` with its result and
//! `exited` with it over its own Owner connection (`aterm_agent::supervise`'s
//! `task_mark`), and a worker's starter (`spawn agent=`) marks it `working`
//! — and a `turn` someone other than the harness types into a done worker
//! ([`reopen_at_turn`]). Any Owner-token holder can `task mark`: the same
//! holders can already type into any session, so a separate authority would
//! protect nothing; the harness is the record's expected writer of `done`
//! and `exited`. Its result is read back with `result` ([`cmd_result`]).
//! The record is four states and the facts that say why:
//!
//! * `working` — the task is the agent's; nothing to read yet (a worker
//!   under Autopilot Cruise stays here for good: Cruise takes no report as
//!   the end, and a manager reads `status`, or moves it to Land);
//! * `done` — the worker LANDED (`why=landed`): under Autopilot Land, the
//!   window harness's verdict (`TurnEndState::landed()`) at a land act's
//!   yield whose reply is a clean `Nothing left:` defense over a repository
//!   holding nothing the remote main lacks — all of the work is on the
//!   remote main branch. An older build's `why=marker|done-check` (a retired
//!   token, a done check) still reads, from a record it carried across an
//!   update, but no harness writes it any more;
//! * `exited` — the agent is gone with no relaunch coming (`exit=` its code);
//! * `stalled` — the worker never showed its composer (a login, a dialog no
//!   rule answers), or, started, is HELD: at a screen the harness has no act
//!   for, nothing moved — its screen, its transcript, a key — for the window
//!   host's held deadline (`why=held`; `working why=moved` once it moves).
//!
//! A question or a wall is NOT a task state: it is the `agent=` word and the
//! `attention=` the session already carries, and the `task` reply joins them
//! rather than copying them.
//!
//! ONE EDGE FOR A WAITER. `rev` moves on every change of the record, AND when
//! the supervisor's own attention entry (`meta set attention owner=supervisor`,
//! [`crate::harness_host::ATTENTION_OWNER`]) or a worker's NEEDS YOU entry
//! (`owner=needs`, [`crate::harness_host::NEEDS_ATTENTION_OWNER`]: a need
//! that never blocks the work) is raised or cleared on a session that has one
//! ([`SessionTimeline::note_task_attention`]: the flag stands while either
//! does). So `await task
//! since=<rev>` is edge-triggered on the one number: a re-armed wait never sees
//! the same news twice, and a wait re-armed across the server's 600 s clamp or
//! a self-update — past each target's rev from the start of the wait (the
//! ctl client's `@<sel>:<rev>`) — misses none. A mark that
//! changes nothing (the same state and the same facts) is a no-op and moves no
//! rev, so a harness that marks `done` again after a restart wakes no one.
//!
//! WHERE IT LIVES: on the session's timeline leaf ([`SessionCtx::timeline`]),
//! beside the agent publication it is read with, so the `task` verb, `status`,
//! `sessions`, `await task` and the `events` digest read it under the one lock
//! they already take, and every change records its `task-change` row (pushed
//! as `EVENT <local> task …`) in the same critical section. A seamless update
//! carries it ([`crate::session_store::SessionRecord::task`]) and the adopting
//! session is seeded before it is registered ([`seed_carried`]).
//!
//! [`SessionCtx::timeline`]: crate::SessionCtx::timeline
//! [`SessionTimeline::note_task_attention`]: crate::session_timeline::SessionTimeline::note_task_attention

use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::SessionCtx;
use crate::control::Scope;
use crate::turn_ledger::TurnBy;

/// A task's state ([the module](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskState {
    /// The task is the agent's; nothing to read yet.
    Working,
    /// The worker landed its work (or, in a record an older build carried,
    /// finished by its retired rule).
    Done,
    /// The agent is gone with no relaunch coming.
    Exited,
    /// The worker never showed its composer, or is held where nothing moves.
    Stalled,
}

impl TaskState {
    /// Every state, in the order the usage line names them.
    pub(crate) const ALL: [Self; 4] = [Self::Working, Self::Done, Self::Exited, Self::Stalled];

    /// The wire word.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Exited => "exited",
            Self::Stalled => "stalled",
        }
    }

    /// The state a wire word names, or `None`.
    #[must_use]
    pub(crate) fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == word)
    }

    /// Whether a manager waiting on the task has something to read: every state
    /// but `working`.
    #[must_use]
    pub(crate) const fn is_terminal(self) -> bool {
        !matches!(self, Self::Working)
    }
}

/// The `why=` of the one `done` a harness writes: Autopilot Land LANDED the
/// work ([`TASK_WHY`]'s first word). Every surface that words a landing
/// reads it to say `landed`, and an older build's `marker|done-check` keeps
/// `done` (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4).
pub(crate) const WHY_LANDED: &str = "landed";

/// The closed `why=` vocabulary: what moved a task to its state. A word
/// outside it is refused, so a reader can switch on it.
pub(crate) const TASK_WHY: &[&str] = &[
    // `done`: Autopilot Land landed the work: a clean `Nothing left:`
    // defense at a land act's yield, over a repository holding nothing the
    // remote main lacks (FOREVER LAND). The one `done` a harness writes.
    WHY_LANDED,
    // `done`, READ ONLY: an older build's retired token rule. A record it
    // carried across a seamless update (or a rollback) still parses; no
    // harness writes it, and it seeds no landing.
    "marker",
    // `done`, READ ONLY: an older build's done check, as `marker` is.
    "done-check",
    // `exited`: the agent exited and no relaunch follows.
    "exit",
    // `stalled`: the shell never showed its first prompt.
    "shell",
    // `stalled`: the agent never showed its composer.
    "startup",
    // `stalled`: the agent started, and is held where nothing moves.
    "held",
    // `working`: a later turn moved a landed task back to work.
    "turn",
    // `working`: a worker the held deadline stalled moved on.
    "moved",
];

/// `from=` when a mark names none and the session has no record yet: a task
/// started by the instance's own client, not by another session.
pub(crate) const FROM_CTL: &str = "ctl";

/// Whether `from` is a task's origin: `ctl`, or a global session id
/// (`s-<hex>`, at most 64 bytes — the shape every sid this build mints).
#[must_use]
pub(crate) fn valid_from(from: &str) -> bool {
    from == FROM_CTL
        || (from.len() <= 64
            && from
                .strip_prefix("s-")
                .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())))
}

/// Where a task's files live: `<state>/tasks/`. A record's `result_path` must
/// name a file in it. `None` when the state dir does not resolve.
#[must_use]
pub(crate) fn tasks_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|state| tasks_dir_under(&state))
}

/// [`tasks_dir`] under the state root `state`: where the window harness
/// snapshots a worker's result (`aterm_agent::harness::result`).
#[must_use]
pub(crate) fn tasks_dir_under(state: &Path) -> PathBuf {
    state.join("tasks")
}

/// The wall clock in unix milliseconds (`0` before the epoch).
fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One session's task record ([the module](self)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRecord {
    /// Moves on every change of the record and on every raise or clear of the
    /// supervisor's attention entry; `1` for a new record.
    pub(crate) rev: u64,
    /// The state.
    pub(crate) state: TaskState,
    /// Who started the task: a global sid, or [`FROM_CTL`].
    pub(crate) from: String,
    /// What moved it to its state ([`TASK_WHY`]), when something said.
    pub(crate) why: Option<&'static str>,
    /// The agent's exit code, for an `exited` task that has one.
    pub(crate) exit: Option<i32>,
    /// The size of the worker's result, `0` before there is one.
    pub(crate) result_bytes: u64,
    /// The file the result is in, under [`tasks_dir`].
    pub(crate) result_path: Option<PathBuf>,
    /// Whether the supervisor's attention entry stands on the session: the
    /// harness escalated something the worker needs a person for.
    pub(crate) attention: bool,
    /// When the state was entered, on the wall clock: what a handoff carries.
    pub(crate) since_unix_ms: u64,
    /// When the state was entered, on the monotonic clock: what `since_ms=`
    /// counts from.
    pub(crate) since: Instant,
    /// The rev the record entered its state at: a mark of the same state
    /// (the result's lag repair, [`repair_result`]) and the attention's edge
    /// move [`Self::rev`] and keep it, so it names ONE entry into the state —
    /// the done a `turn`'s Enter found ([`reopen_at_turn`]). Not on the wire.
    pub(crate) entered: u64,
    /// THE AGENT A WORKER'S SPAWN STARTED (`spawn agent=<word>`,
    /// `App::start_worker`; `None` for a record a `task mark` made): whose
    /// reader the startup deadline reads the worker's screen by before the
    /// tab publishes an agent (`harness_host::startup_seen`), whose product
    /// its stall names, and what the window harness reads as the worker's
    /// agent (`meta`'s `worker=`). A mark keeps it; a seamless update
    /// carries it ([`Self::carry_row`]). Not on the wire.
    pub(crate) agent: Option<aterm_phase::Program>,
    /// THE ONE FOLDER A WORKER'S STARTER TRUSTED FOR IT: the canonical `cwd=`
    /// an Owner's `spawn agent=<word>` named (`App::start_worker`), which the
    /// window harness's folder-trust rule trusts for this session alone
    /// (`meta`'s `trusted_cwd=`; `[harness] trust_roots` is not widened).
    /// Written by the spawn alone, never by a mark or `meta set`: no driver
    /// can name a folder trusted. A mark keeps it, and a seamless update
    /// carries it with the record ([`Self::carry_row`]): a worker adopted
    /// across one keeps the trust its spawn gave it, and its agent's
    /// folder-trust dialog — met again on a relaunch there — is answered in
    /// that folder as before. `None` for a record a `task mark` made. Not on
    /// the wire.
    pub(crate) trusted_cwd: Option<String>,
}

impl TaskRecord {
    /// Milliseconds since the state was entered (`since_ms=`).
    #[must_use]
    pub(crate) fn since_ms(&self) -> u64 {
        u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Whether a waiter has news: a state to read, or a person needed.
    #[must_use]
    pub(crate) fn has_news(&self) -> bool {
        self.state.is_terminal() || self.attention
    }

    /// The record's reply fields: `rev= state= from= since_ms= result_bytes=`,
    /// then `why=`, `exit=` and `result_path=` (pct-encoded) when set.
    #[must_use]
    pub(crate) fn wire_fields(&self) -> String {
        let mut out = format!(
            "rev={} state={} from={} since_ms={} result_bytes={}",
            self.rev,
            self.state.as_str(),
            self.from,
            self.since_ms(),
            self.result_bytes
        );
        self.push_optional(&mut out);
        out
    }

    /// ` why= exit= result_path=`, each only when set.
    fn push_optional(&self, out: &mut String) {
        if let Some(why) = self.why {
            out.push_str(" why=");
            out.push_str(why);
        }
        if let Some(exit) = self.exit {
            out.push_str(&format!(" exit={exit}"));
        }
        if let Some(path) = &self.result_path {
            out.push_str(" result_path=");
            out.push_str(&crate::control::pct_encode(&path.to_string_lossy()));
        }
    }

    /// The `task-change` timeline row's payload, which the `events` digest
    /// pushes verbatim as `EVENT <local> task <payload>`: `rev= state=
    /// attention=<0|1>`, then `why=` and `exit=` when set. No path and no
    /// size: a watcher learns THAT the task moved, and reads it with `task`.
    #[must_use]
    pub(crate) fn event_payload(&self) -> String {
        let mut out = format!(
            "rev={} state={} attention={}",
            self.rev,
            self.state.as_str(),
            u8::from(self.attention)
        );
        if let Some(why) = self.why {
            out.push_str(" why=");
            out.push_str(why);
        }
        if let Some(exit) = self.exit {
            out.push_str(&format!(" exit={exit}"));
        }
        out
    }

    /// The row a seamless update carries ([`crate::session_store::SessionRecord::task`]):
    /// `rev= state= from= since=<unix ms> result_bytes= attention=<0|1>`, then
    /// `why=`, `exit=` and `result_path=` when set, and `agent=` and
    /// `trusted_cwd=` (pct-encoded) a worker's ([`Self::agent`],
    /// [`Self::trusted_cwd`]). [`Self::from_carry_row`] reads it back.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn carry_row(&self) -> String {
        let mut out = format!(
            "rev={} state={} from={} since={} result_bytes={} attention={}",
            self.rev,
            self.state.as_str(),
            self.from,
            self.since_unix_ms,
            self.result_bytes,
            u8::from(self.attention)
        );
        self.push_optional(&mut out);
        if let Some(agent) = self.agent {
            out.push_str(" agent=");
            out.push_str(agent.name());
        }
        if let Some(folder) = &self.trusted_cwd {
            out.push_str(" trusted_cwd=");
            out.push_str(&crate::control::pct_encode(folder));
        }
        out
    }

    /// Read a carried row back, re-validated as the verb validates a mark (a
    /// `result_path` must name a file under `tasks`, [`tasks_dir`]): a row
    /// missing a key this build needs, or holding one it cannot read, is
    /// `None` — the session crosses with no record, never with a made-up one.
    /// A key this build does not know is skipped: the row a NEWER build wrote
    /// still reads here after a rollback, as the manifest it rides does — and
    /// so is an `agent=` word it does not know (an agent a newer build
    /// added): the record crosses, read as a `task mark`'s. A `trusted_cwd=`
    /// that is no absolute folder spelled as its own parts ([`trusted_folder`])
    /// crosses as none: the record keeps its facts, and no folder is trusted
    /// on a word this build cannot vouch for.
    /// `now`/`now_unix_ms` re-anchor the monotonic `since` so `since_ms=`
    /// keeps counting from when the state was entered.
    #[must_use]
    pub(crate) fn from_carry_row(
        row: &str,
        tasks: Option<&Path>,
        now: Instant,
        now_unix_ms: u64,
    ) -> Option<Self> {
        // Every key once: a row naming one twice says two things, and neither
        // is the record. A bare word is no key, and is skipped as one.
        let mut fields: Vec<(&str, &str)> = Vec::new();
        for (key, value) in row.split_whitespace().filter_map(|t| t.split_once('=')) {
            if fields.iter().any(|(k, _)| *k == key) {
                return None;
            }
            fields.push((key, value));
        }
        let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let since_unix_ms = field("since")?.parse::<u64>().ok()?;
        let age = std::time::Duration::from_millis(now_unix_ms.saturating_sub(since_unix_ms));
        let rev = field("rev")?.parse::<u64>().ok().filter(|rev| *rev > 0)?;
        Some(Self {
            rev,
            state: TaskState::parse(field("state")?)?,
            from: Some(field("from")?).filter(|f| valid_from(f))?.to_string(),
            why: match field("why") {
                Some(word) => Some(why_word(word)?),
                None => None,
            },
            exit: match field("exit") {
                Some(code) => Some(code.parse::<i32>().ok()?),
                None => None,
            },
            result_bytes: field("result_bytes")?.parse::<u64>().ok()?,
            result_path: match field("result_path") {
                Some(path) => Some(result_path(path, tasks)?),
                None => None,
            },
            attention: match field("attention")? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            since_unix_ms,
            since: now.checked_sub(age).unwrap_or(now),
            // The rev it entered its state at is not carried: this process's
            // own entry into it is the one a turn here can find.
            entered: rev,
            agent: field("agent").and_then(worker_agent),
            trusted_cwd: field("trusted_cwd").and_then(trusted_folder),
        })
    }
}

/// The folder a carried `trusted_cwd=` names: pct-decoded, absolute, spelled
/// as its own parts ([`spelled_as_its_parts`]: no `.`, `..`, doubled or
/// trailing separator), no control character, at most [`RESULT_PATH_MAX`]
/// bytes — the canonical form the spawn stored. Anything else is none.
fn trusted_folder(value: &str) -> Option<String> {
    let decoded = aterm_codec::pct::decode(value);
    let path = Path::new(&decoded);
    (decoded.len() <= RESULT_PATH_MAX
        && !decoded.chars().any(char::is_control)
        && path.has_root()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
        && spelled_as_its_parts(path))
    .then(|| decoded.clone())
}

/// The agent a worker's `agent=` word names: one a spawn can start
/// (`aterm_agent::harness::worker::worker_cli`), else none.
fn worker_agent(word: &str) -> Option<aterm_phase::Program> {
    aterm_phase::Program::ALL
        .into_iter()
        .find(|p| p.name() == word)
        .filter(|p| aterm_agent::harness::worker::worker_cli(*p).is_some())
}

/// The [`TASK_WHY`] word `value` is, as its `'static` spelling.
fn why_word(value: &str) -> Option<&'static str> {
    TASK_WHY.iter().copied().find(|w| *w == value)
}

/// One mark: the state a task is moved to and the facts that describe it. A
/// mark is the WHOLE new state — a fact it leaves out is cleared, except
/// `from`, which keeps the record's when none is given — so marking the same
/// state with the same facts twice is one change, not two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskMark {
    /// The state.
    pub(crate) state: TaskState,
    /// Who started the task; `None` keeps the record's ([`FROM_CTL`] for a new one).
    pub(crate) from: Option<String>,
    /// What moved it ([`TASK_WHY`]).
    pub(crate) why: Option<&'static str>,
    /// The agent's exit code.
    pub(crate) exit: Option<i32>,
    /// The result's size.
    pub(crate) result_bytes: u64,
    /// The result's file, under [`tasks_dir`].
    pub(crate) result_path: Option<PathBuf>,
}

/// `task`'s usage line.
pub(crate) const TASK_USAGE: &str = "ERR usage: task [mark <working|done|exited|stalled> \
     [from=<s-…|ctl>] [why=<word>] [exit=<n>] [result_bytes=<n>] [result_path=<pct>]]\n";

impl TaskMark {
    /// A mark to `state` with no facts.
    #[must_use]
    pub(crate) fn new(state: TaskState) -> Self {
        Self {
            state,
            from: None,
            why: None,
            exit: None,
            result_bytes: 0,
            result_path: None,
        }
    }

    /// Parse `mark`'s arguments: `<state> [from=] [why=] [exit=] [result_bytes=]
    /// [result_path=<pct>]`, each key at most once. `tasks` is the directory a
    /// `result_path` must name a file in ([`tasks_dir`]). `Err` is the reply line.
    pub(crate) fn parse(args: &str, tasks: Option<&Path>) -> Result<Self, String> {
        let usage = || TASK_USAGE.to_string();
        let mut words = args.split_whitespace();
        let state = words.next().and_then(TaskState::parse).ok_or_else(usage)?;
        let mut mark = Self::new(state);
        let mut seen: Vec<&str> = Vec::new();
        for token in words {
            let (key, value) = token.split_once('=').ok_or_else(usage)?;
            if seen.contains(&key) {
                return Err(usage());
            }
            seen.push(key);
            match key {
                "from" if valid_from(value) => mark.from = Some(value.to_string()),
                "from" => {
                    return Err("ERR task mark: from= is a session id (s-<hex>) or ctl\n".into());
                }
                "why" => {
                    mark.why = Some(why_word(value).ok_or_else(|| {
                        format!("ERR task mark: why= is one of {}\n", TASK_WHY.join("|"))
                    })?);
                }
                "exit" => mark.exit = Some(value.parse::<i32>().map_err(|_| usage())?),
                "result_bytes" => mark.result_bytes = value.parse::<u64>().map_err(|_| usage())?,
                "result_path" => {
                    mark.result_path = Some(result_path(value, tasks).ok_or_else(|| {
                        "ERR task mark: result_path= names a file under <state>/tasks/\n"
                            .to_string()
                    })?);
                }
                _ => return Err(usage()),
            }
        }
        Ok(mark)
    }

    /// Apply this mark to the record `current` at `now` (`now_unix_ms` on the
    /// wall clock), with the supervisor's attention as `attention` reads it
    /// now. Returns the record after it and whether it moved: a new record is
    /// rev 1, no worker's ([`TaskRecord::agent`]); a change bumps the rev, and
    /// a change of state restarts `since`; the same state with the same facts
    /// changes nothing. The record's agent and trusted folder are kept.
    #[must_use]
    pub(crate) fn apply(
        &self,
        current: Option<&TaskRecord>,
        attention: bool,
        now: Instant,
        now_unix_ms: u64,
    ) -> (TaskRecord, bool) {
        let from = self
            .from
            .clone()
            .or_else(|| current.map(|r| r.from.clone()))
            .unwrap_or_else(|| FROM_CTL.to_string());
        let Some(current) = current else {
            let record = TaskRecord {
                rev: 1,
                state: self.state,
                from,
                why: self.why,
                exit: self.exit,
                result_bytes: self.result_bytes,
                result_path: self.result_path.clone(),
                attention,
                since_unix_ms: now_unix_ms,
                since: now,
                entered: 1,
                agent: None,
                trusted_cwd: None,
            };
            return (record, true);
        };
        let same = current.state == self.state
            && current.from == from
            && current.why == self.why
            && current.exit == self.exit
            && current.result_bytes == self.result_bytes
            && current.result_path == self.result_path
            && current.attention == attention;
        let mut next = current.clone();
        if same {
            return (next, false);
        }
        next.rev = next.rev.saturating_add(1);
        if next.state != self.state {
            next.since = now;
            next.since_unix_ms = now_unix_ms;
            next.entered = next.rev;
        }
        next.state = self.state;
        next.from = from;
        next.why = self.why;
        next.exit = self.exit;
        next.result_bytes = self.result_bytes;
        next.result_path.clone_from(&self.result_path);
        next.attention = attention;
        (next, true)
    }
}

/// The longest `result_path=` a record holds, decoded: macOS's `PATH_MAX`.
/// The path rides every `task` reply, handoff record and carried row, so a
/// value no file could have is refused rather than carried.
pub(crate) const RESULT_PATH_MAX: usize = 1024;

/// A `result_path=` value as a path: pct-decoded, at most
/// [`RESULT_PATH_MAX`] bytes, absolute, spelled the one way its components
/// spell it (no `.`, no `//`, no trailing `/`; [`spelled_as_its_parts`]), and
/// naming a file strictly inside `tasks` with no `..` or control character on
/// the way. Lexical only — the file need not exist yet, and nothing here reads
/// it. The path is kept as it was spelled: the wire carries this platform's
/// own spelling (`C:\…\tasks\…` on Windows), so a record's row reads back to
/// the same bytes.
fn result_path(value: &str, tasks: Option<&Path>) -> Option<PathBuf> {
    let tasks = tasks?;
    let decoded = aterm_codec::pct::decode(value);
    if decoded.len() > RESULT_PATH_MAX || decoded.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(decoded);
    if !spelled_as_its_parts(&path) {
        return None;
    }
    let inside = path.strip_prefix(tasks).ok()?;
    let mut parts = inside.components().peekable();
    parts.peek()?;
    parts
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(path)
}

/// Whether `path` is spelled as the path its own components rebuild: byte for
/// byte off Windows, so a `.`, a `//` or a trailing `/` is a second spelling
/// of one file. On Windows `/` and `\` both separate, and a state root spelled
/// with `/` (`ATERM_STATE_HOME=C:/…`) joins `tasks\<file>` onto it with `\`,
/// so there the two are one separator — every other difference still counts.
fn spelled_as_its_parts(path: &Path) -> bool {
    let rebuilt: PathBuf = path.components().collect();
    #[cfg(windows)]
    {
        let one_separator = |p: &Path| p.to_string_lossy().replace('/', "\\");
        one_separator(&rebuilt) == one_separator(path)
    }
    #[cfg(not(windows))]
    {
        rebuilt.as_os_str() == path.as_os_str()
    }
}

/// Whether an attention entry written by `owner` moves the task record's
/// attention flag ([`supervisor_attention`]): the supervisor's own, and a
/// worker's NEEDS YOU entry.
pub(crate) fn owner_moves_task(owner: &str) -> bool {
    owner == crate::harness_host::ATTENTION_OWNER
        || owner == crate::harness_host::NEEDS_ATTENTION_OWNER
}

/// Whether the task record's attention stands in `meta`: the supervisor's
/// own entry, or a worker's NEEDS YOU entry (`owner=needs`, which never
/// blocks the work but is news a manager waits on).
pub(crate) fn supervisor_attention(meta: &crate::session_timeline::SessionMeta) -> bool {
    [
        crate::harness_host::ATTENTION_OWNER,
        crate::harness_host::NEEDS_ATTENTION_OWNER,
    ]
    .iter()
    .any(|owner| meta.attention_owners.get(owner).is_some())
}

/// A mark that waited out the session's fence bound
/// ([`crate::fabric::SessionFabric::fence_wait`]) behind a committing update's
/// fence and changed nothing ([`mark_session_task`]): the caller answers
/// [`crate::fabric::HOLD_BUSY`], and a retry reaches whichever process then
/// owns the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkFenced;

/// MARK A SESSION'S TASK — the in-process API every writer of the record
/// goes through: `task mark` today, and the seam the spawn path (`spawn
/// agent=claude` marking `working` with its caller as `from=`) and the window
/// harness (`done` at its verdict, `exited` when no relaunch follows) are to
/// call. Applies `mark` to `ctx`'s record ([`TaskMark::apply`]), records the
/// `task-change` row when it moved, and wakes every waiter on `local_id`
/// (`await task`, the `events` digest) at once. Returns the record after the
/// mark and whether it moved.
///
/// A COMMITTING UPDATE HOLDS THE MARK BACK, as it holds a `topic` change
/// (`fabric::cmd_topic`): the successor carries the record the park drew, and
/// the Commit compares it again under its fence (`App::carried_claims_held`),
/// so a mark that lands first stands that Commit down and one after it waits
/// — the update that lands `_exit`s with it waiting, and one that stands down
/// lets it land here. Past its [`crate::fabric::SessionFabric::fence_wait`] it is
/// [`MarkFenced`], with nothing moved. A worker marked `done` is therefore
/// never answered `OK` by a process whose successor crosses it `working`. The
/// main thread raises and lifts that fence itself, so a caller there passes a
/// zero wait ([`mark_session_task_within`]), as the menu's Hold does.
///
/// Under the fence's guard it takes the meta lock, then the timeline lock
/// under it (the documented meta → timeline order), so the attention flag it
/// writes is the one a concurrent `meta set attention owner=supervisor`
/// leaves.
pub(crate) fn mark_session_task(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
) -> Result<(TaskRecord, bool), MarkFenced> {
    mark_session_task_within(ctx, local_id, mark, ctx.fabric.fence_wait())
}

/// [`mark_session_task`], waiting at most `fence_wait` on a committing
/// update's fence: zero on the main thread, which raises the fence itself, and
/// in a test that asks at once whether it stands.
pub(crate) fn mark_session_task_within(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
) -> Result<(TaskRecord, bool), MarkFenced> {
    let marked = mark_session_task_if(ctx, local_id, mark, fence_wait, |_| true)?;
    Ok(marked.expect("an unconditional mark is applied"))
}

/// [`mark_session_task_within`] applied only where `when` holds of the
/// record as it stands, read under the same locks the mark takes: a move
/// conditional on the state (a `done` worker opened again by a turn,
/// [`reopen_at_turn`]) that a mark landing in between — the host's `exited`
/// — is never overwritten by. `Ok(None)`: `when` did not hold, nothing moved.
pub(crate) fn mark_session_task_if(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
    when: impl FnOnce(Option<&TaskRecord>) -> bool,
) -> Result<Option<(TaskRecord, bool)>, MarkFenced> {
    let marked = mark_session_task_with(
        ctx,
        local_id,
        Some(mark),
        AttentionEdit::Keep,
        fence_wait,
        |record, _| when(record),
    )?;
    Ok(marked.and_then(|m| m.record.map(|record| (record, m.moved))))
}

/// What a mark does to the supervisor's own attention entry
/// ([`crate::harness_host::ATTENTION_OWNER`]) in its critical section
/// ([`mark_session_task_with`]).
#[derive(Clone, Copy)]
pub(crate) enum AttentionEdit<'a> {
    /// Leave it as it stands.
    Keep,
    /// Raise it with this text (whatever stands: a caller that must not
    /// replace another writer's text says so in its `when`).
    Raise(&'a str),
    /// Take it down where `stands` holds of the text standing (another
    /// writer's text is theirs and stays).
    TakeDown(fn(Option<&str>) -> bool),
}

/// What [`mark_session_task_with`] left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkedWith {
    /// The record after it (`None`: the session has no task).
    pub(crate) record: Option<TaskRecord>,
    /// The record moved: one new rev.
    pub(crate) moved: bool,
    /// The supervisor's attention entry moved: the chrome that shows it is
    /// owed a repaint (`presence::post_meta_changed`).
    pub(crate) attention_moved: bool,
}

/// A MARK AND THE SUPERVISOR'S ATTENTION, ONE EDGE: where `when` holds of the
/// record as it stands, the attention edit is made and then `mark` applied
/// (`None`: the record keeps its state, its attention flag followed), all
/// under the one meta guard and timeline lock — so the record moves ONCE,
/// its state and its `attention=` together, and an `await task` woken by it
/// reads both: never the attention's rev with the old state, nor the new
/// state with the attention still to come. What the window host's deadlines
/// mark with (`harness_host::startup_mark_with`): a stall with its attention
/// raised, a start or a clear with it taken down. `when` reads the record and
/// the supervisor's attention entry standing on it, so a raise can be made
/// conditional on what it would replace. Held back behind a
/// committing update's fence as [`mark_session_task`] is; waiters woken once
/// when anything moved, and the window host rung when the record entered a
/// state. `Ok(None)`: `when` did not hold, nothing moved.
pub(crate) fn mark_session_task_with(
    ctx: &SessionCtx,
    local_id: u64,
    mark: Option<&TaskMark>,
    edit: AttentionEdit<'_>,
    fence_wait: std::time::Duration,
    when: impl FnOnce(Option<&TaskRecord>, Option<&str>) -> bool,
) -> Result<Option<MarkedWith>, MarkFenced> {
    let outcome = ctx
        .fabric
        .run_unfenced(fence_wait, || {
            let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let owner = crate::harness_host::ATTENTION_OWNER;
            if !when(timeline.task(), meta.attention_owners.get(owner)) {
                return None;
            }
            let attention_moved = match edit {
                AttentionEdit::Keep => false,
                AttentionEdit::Raise(text) => crate::session_timeline::put_supervisor_attention(
                    &mut meta,
                    &mut timeline,
                    Some(text),
                ),
                AttentionEdit::TakeDown(stands) => {
                    stands(meta.attention_owners.get(owner))
                        && crate::session_timeline::put_supervisor_attention(
                            &mut meta,
                            &mut timeline,
                            None,
                        )
                }
            };
            let attention = supervisor_attention(&meta);
            let (record, moved) = match mark {
                Some(mark) => {
                    let (record, moved) =
                        timeline.mark_task(mark, attention, Instant::now(), unix_ms_now());
                    (Some(record), moved)
                }
                None => {
                    let moved = timeline.note_task_attention(attention);
                    (timeline.task().cloned(), moved)
                }
            };
            // The rows this hold recorded (`task-change`, `meta-change`)
            // rang the session's watchers; the session log's owed wake
            // rings them here, under this lock.
            timeline.slog_take_wake();
            drop(timeline);
            drop(meta);
            Some(MarkedWith {
                record,
                moved,
                attention_moved,
            })
        })
        .ok_or(MarkFenced)?;
    let record_moved = outcome
        .as_ref()
        .is_some_and(|m| m.moved || m.attention_moved);
    // The main thread's need verdict, which reads the record
    // (`Wake::TaskChanged`): only when the record moved, never for the
    // log's wake alone.
    if record_moved {
        crate::presence::post_task_changed(local_id);
    }
    // A record that ENTERED a state is news to the window host's deadlines,
    // which judge a worker by its record's state (`harness_host::
    // look_at_startups`): a `turn` that opened a done worker again lands
    // after the screen's own ring, and its look must not wait for the next.
    if outcome
        .as_ref()
        .is_some_and(|m| m.moved && m.record.as_ref().is_some_and(|r| r.entered == r.rev))
    {
        crate::harness_host::ring();
    }
    Ok(outcome)
}

/// A TURN INTO A FINISHED WORKER (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
/// §2.2): a `turn` that landed in session `local_id` whose task is `done`,
/// typed by anyone but the window harness — the turn ledger's `by=`
/// ([`TurnBy`]: a manager's follow-up, a person's `aterm ctl`, a peer's) —
/// moves the task back to `working why=turn` AT ONCE, before the worker's
/// next turn ends: a manager re-arming `await task` past the done rev waits
/// for the follow-up's own `done`. The live upgrade's READY notice and a
/// relaunched agent's continuation are typed under the harness's hand
/// (`by=harness`) and change nothing. The harness marks the same where its
/// own `done` ends on someone else's turn (`aterm_agent::supervise`'s
/// `task_mark`, which then publishes that turn's done). Only THE DONE THE
/// TURN'S ENTER FOUND opens (`found`, the rev that record entered its state
/// at, [`TaskRecord::entered`]): that done marked again since — the result's
/// lag repair a `result` makes ([`repair_result`]), the supervisor's
/// attention raised or cleared, each a new rev of the same done — still
/// opens, so the turn's own done is the next news past the rev its verdict
/// names; a done entered since — the harness's own `working` and `done` for
/// this very turn landing first — or an `exited` is left as it stands, read
/// again under the mark's own locks ([`mark_session_task_if`]), so this
/// turn's published done is never opened again by its own submit. A record
/// in any other state, and none, is left as it stands.
pub(crate) fn reopen_at_turn(ctx: &SessionCtx, local_id: u64, by: &TurnBy, found: Option<u64>) {
    let is_done = |r: Option<&TaskRecord>| {
        r.is_some_and(|r| r.state == TaskState::Done && Some(r.entered) == found)
    };
    // A cheap look first: a turn into a session with no finished task never
    // waits on a committing update's fence.
    if *by == TurnBy::Harness
        || !is_done(
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task(),
        )
    {
        return;
    }
    let mut mark = TaskMark::new(TaskState::Working);
    mark.why = Some("turn");
    let fence = ctx.fabric.fence_wait();
    if mark_session_task_if(ctx, local_id, &mark, fence, is_done).is_err() {
        aterm_log::warn!(
            "task: a turn into a finished worker was not marked working: an update is committing"
        );
    }
}

/// `result`'s usage line.
pub(crate) const RESULT_USAGE: &str = "ERR usage: result [full]\n";

/// `result [full]` on `ctx`: THE WORKER'S RESULT
/// (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.3), the snapshot the
/// window harness took at the landing (the report before the land act and
/// its clean `Nothing left:` defense) and named in the record
/// (`result_path=`), answered by [`aterm_agent::harness::result::answer`]:
/// `OK <n> source=transcript session=<id> uuid=<row> bytes=<n>` and the
/// text as lines, at most 24,000 characters of it, then `… truncated; full:
/// <path>`; `result full` the whole exchange since the task's user row,
/// read from the transcript now and bounded. A done task whose result
/// could not be read answers `ERR result unavailable why=<word>`. No task:
/// `ERR no task`; a task with no result (yet): `ERR result unavailable
/// why=<its state>` (`working`, `stalled`, `exited`), `why=no-snapshot` for
/// a `done` marked with none. An `exited` task the harness had marked done
/// keeps its result, and answers it.
///
/// A RESULT THE DONE EDGE MISSED to Claude Code's write lag (the snapshot's
/// `why=no-reply`, or an older build's `no-marker`, its sidecar naming the
/// turn's user row) is read
/// again for THAT turn ([`aterm_agent::harness::result::reread`]) — never a
/// later one — and, found, answered; it then becomes the result for good
/// ([`repair_result`]).
pub(crate) fn cmd_result(ctx: &SessionCtx, local_id: u64, rest: &str) -> String {
    let full = match rest.trim() {
        "" => false,
        "full" => true,
        _ => return RESULT_USAGE.to_string(),
    };
    let record = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .cloned();
    let Some(record) = record else {
        return "ERR no task\n".to_string();
    };
    match (&record.result_path, record.state) {
        (Some(path), TaskState::Done | TaskState::Exited) => {
            match aterm_agent::harness::result::reread(path) {
                Some(found) => {
                    repair_result(ctx, local_id, &record, path, &found);
                    found.answer(path, full)
                }
                None => aterm_agent::harness::result::answer(path, full),
            }
        }
        (_, TaskState::Done) => "ERR result unavailable why=no-snapshot\n".to_string(),
        (_, state) => format!("ERR result unavailable why={}\n", state.as_str()),
    }
}

/// THE MISSED RESULT, KEPT: the record `record` (as `result` read it) marked
/// again with `found`'s size as its `result_bytes=` — every other fact as it
/// stands, a new rev, its waiters woken — then the snapshot at `path` and its
/// sidecar written over with it, so `task`, `await task` and every later
/// `result` agree. The mark is conditional on the record being the one read
/// (the same rev: a `turn` that opened the task again, or a mark that landed
/// meanwhile, stands), and is not waited on behind a committing update: then
/// nothing is written, the reply still carries the report, and the next
/// `result` — the successor's, after an update — reads the turn again.
fn repair_result(
    ctx: &SessionCtx,
    local_id: u64,
    record: &TaskRecord,
    path: &Path,
    found: &aterm_agent::harness::result::Reread,
) {
    let mark = TaskMark {
        state: record.state,
        from: Some(record.from.clone()),
        why: record.why,
        exit: record.exit,
        result_bytes: found.bytes(),
        result_path: record.result_path.clone(),
    };
    let same = |now: Option<&TaskRecord>| now.is_some_and(|now| now.rev == record.rev);
    match mark_session_task_if(ctx, local_id, &mark, std::time::Duration::ZERO, same) {
        Ok(Some(_)) => {
            if let Err(e) = found.write(path) {
                aterm_log::warn!(
                    "task: the result read again was not written over {}: {e}",
                    path.display()
                );
            }
        }
        Ok(None) => {}
        Err(MarkFenced) => aterm_log::warn!(
            "task: the result read again is not kept while an update commits; the next `result` \
             reads it again"
        ),
    }
}

/// Put back the record a seamless update carried (`row`, the handoff
/// record's [`TaskRecord::carry_row`]) on the adopting session's timeline,
/// before it is registered and AFTER its carried claims are back on its meta
/// (`session_timeline::seed_carried_claims`). A row that does not read seeds
/// nothing: the session crosses with no task rather than a made-up one. The
/// supervisor's attention flag is read off that seeded meta, never off the
/// row: a claim the seed refused (unreadable, or past the owners' cap) is no
/// attention here, so the record never reports news no meta entry stands for.
pub(crate) fn seed_carried(ctx: &SessionCtx, row: Option<&str>) {
    let Some(row) = row else {
        return;
    };
    let Some(mut record) =
        TaskRecord::from_carry_row(row, tasks_dir().as_deref(), Instant::now(), unix_ms_now())
    else {
        aterm_log::warn!(
            "task: the carried task record does not read ({row:?}); the session crosses without one"
        );
        return;
    };
    record.attention = supervisor_attention(&ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .seed_task(record);
    // A worker an older build started, with no word: Land, as a spawn now
    // writes it.
    crate::session_timeline::keep_worker_autopilot(ctx);
}

/// `agent=<word> attention=<pct|->`: the session's published agent verdict
/// and its effective attention entry, as `sessions` prints them (the `task`
/// reply and every `await task` row). Reads the meta, then the timeline,
/// never one under the other.
pub(crate) fn agent_and_attention(ctx: &SessionCtx) -> (&'static str, String) {
    let attention = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attention
        .as_deref()
        .map_or_else(|| "-".to_string(), crate::control::pct_encode);
    let agent = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .word;
    (agent, attention)
}

/// The `task` reply line for `record`, unterminated: `OK task -` with none,
/// else `OK task <fields> agent= attention=`.
fn task_reply(ctx: &SessionCtx, record: Option<&TaskRecord>) -> String {
    let Some(record) = record else {
        return "OK task -".to_string();
    };
    let (agent, attention) = agent_and_attention(ctx);
    format!(
        "OK task {} agent={agent} attention={attention}",
        record.wire_fields()
    )
}

/// `task` / `task mark …` on `ctx` (session `local_id`), for a connection of
/// `scope`.
///
/// * `task` — the record: `OK task rev= state= from= since_ms= result_bytes=
///   [why=] [exit=] [result_path=] agent= attention=`, or `OK task -`.
/// * `task mark <state> [from=] [why=] [exit=] [result_bytes=]
///   [result_path=]` — Owner-only (an edge gets `ERR denied`; the dispatch
///   gate refuses it first, through the `task mark` form's `owner_only` in
///   `aterm_types::control_verbs`): [`mark_session_task`],
///   answered with the record after it and ` changed=<0|1>`, or
///   [`crate::fabric::HOLD_BUSY`] when a committing update held it back. A
///   mark to anything but a deadline's stall (`stalled why=startup|held`)
///   takes a deadline's attention down in the same edge
///   ([`mark_session_task_with`]): a done the worker's harness publishes over
///   such a stall is one rev carrying `attention=-`, never the done with the
///   stall's text still standing and a second rev to take it down (the
///   window host's `StartupMark::Over` stays the backstop).
pub(crate) fn cmd_task(ctx: &SessionCtx, local_id: u64, scope: Scope, rest: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        let record = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .cloned();
        return format!("{}\n", task_reply(ctx, record.as_ref()));
    }
    let Some(args) = rest
        .strip_prefix("mark")
        .filter(|a| a.is_empty() || a.starts_with(' '))
    else {
        return TASK_USAGE.to_string();
    };
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    let parsed = match TaskMark::parse(args, tasks_dir().as_deref()) {
        Ok(parsed) => parsed,
        Err(reply) => return reply,
    };
    let deadline_stall =
        parsed.state == TaskState::Stalled && matches!(parsed.why, Some("startup" | "held"));
    let edit = if deadline_stall {
        AttentionEdit::Keep
    } else {
        AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands)
    };
    let Ok(marked) = mark_session_task_with(
        ctx,
        local_id,
        Some(&parsed),
        edit,
        ctx.fabric.fence_wait(),
        |_, _| true,
    ) else {
        return crate::fabric::HOLD_BUSY.to_string();
    };
    let marked = marked.expect("an unconditional mark is applied");
    if marked.attention_moved {
        crate::presence::post_meta_changed(local_id);
    }
    format!(
        "{} changed={}\n",
        task_reply(ctx, marked.record.as_ref()),
        u8::from(marked.moved)
    )
}

/// How a wait over several targets ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Combine {
    /// At the first target with news (the default).
    Any,
    /// Once every target has news.
    All,
}

/// The most targets one `await task` waits on.
pub(crate) const MAX_AWAIT_TARGETS: usize = 64;

/// `await task`'s usage line.
pub(crate) const AWAIT_TASK_USAGE: &str = "ERR usage: await task [@<sel>[:<rev>][,@<sel>…]] \
     [any|all] [since=<rev>] [timeout=<ms>]\n";

/// One target of an `await task`, as written: the selector body after its
/// `@`, and the rev it waits past when it names its own (`@<sel>:<rev>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetArg {
    /// The selector body (`.`, a local id, an `s-…` sid).
    pub(crate) selector: String,
    /// This target's own `since`, over the request's.
    pub(crate) since: Option<u64>,
}

/// A parsed `await task` request (after the `task` word).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AwaitTaskArgs {
    /// The target list; empty for the single form (the session the request
    /// resolved to).
    pub(crate) targets: Vec<TargetArg>,
    /// `any` (default) or `all`.
    pub(crate) combine: Combine,
    /// The rev every target without its own waits past; `None`: each target's
    /// rev when the wait is armed.
    pub(crate) since: Option<u64>,
    /// `timeout=<ms>` / `timeout <ms>`, clamped to the server's 600 s; `None`
    /// when not given.
    pub(crate) timeout_ms: Option<u64>,
}

/// Parse `await task`'s arguments (the words after `task`). `list` says
/// whether a target list may appear (the selector-less form); with a leading
/// selector the request has its one target already, and a list is usage.
pub(crate) fn parse_await_task(words: &[&str], list: bool) -> Result<AwaitTaskArgs, String> {
    let usage = || AWAIT_TASK_USAGE.to_string();
    let mut args = AwaitTaskArgs {
        targets: Vec::new(),
        combine: Combine::Any,
        since: None,
        timeout_ms: None,
    };
    let (mut combine_seen, mut list_seen) = (false, false);
    let mut words = words.iter().copied();
    while let Some(word) = words.next() {
        if word.starts_with('@') {
            if !list || list_seen {
                return Err(usage());
            }
            list_seen = true;
            for element in word.split(',') {
                let body = element.strip_prefix('@').ok_or_else(usage)?;
                let (selector, since) = match body.split_once(':') {
                    Some((selector, rev)) => {
                        (selector, Some(rev.parse::<u64>().map_err(|_| usage())?))
                    }
                    None => (body, None),
                };
                // `@$W1,@$W2` with one empty is refused, never read as the
                // front session (`aterm_types::control_verbs::EMPTY_SELECTOR`).
                if !aterm_types::control_verbs::selector_body_names_something(selector) {
                    return Err(format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR));
                }
                if args.targets.len() == MAX_AWAIT_TARGETS {
                    return Err(format!(
                        "ERR too many await task targets (max {MAX_AWAIT_TARGETS})\n"
                    ));
                }
                args.targets.push(TargetArg {
                    selector: selector.to_string(),
                    since,
                });
            }
        } else if matches!(word, "any" | "all") {
            if combine_seen {
                return Err(usage());
            }
            combine_seen = true;
            args.combine = if word == "all" {
                Combine::All
            } else {
                Combine::Any
            };
        } else if let Some(rev) = word.strip_prefix("since=") {
            if args.since.is_some() {
                return Err(usage());
            }
            args.since = Some(rev.parse::<u64>().map_err(|_| usage())?);
        } else {
            let value = match word.strip_prefix("timeout=") {
                Some(value) => value,
                None if word == "timeout" => words.next().ok_or_else(usage)?,
                None => return Err(usage()),
            };
            if args.timeout_ms.is_some() {
                return Err(usage());
            }
            args.timeout_ms = Some(aterm_control::wire::wait_ms(value).ok_or_else(usage)?);
        }
    }
    Ok(args)
}

/// One `await task` reply row: `@<gsid> rev=<n> state=<s> agent=<word>
/// attention=<pct|-> result_bytes=<n>` — `rev=0 state=-` for a session with no
/// task, and `state=closed` for one whose session closed while it was waited
/// on (its record went with it).
#[must_use]
pub(crate) fn await_row(
    sid: &str,
    record: Option<&TaskRecord>,
    agent: &str,
    attention: &str,
    closed: bool,
) -> String {
    let (rev, state, result_bytes) =
        record.map_or((0, "-", 0), |r| (r.rev, r.state.as_str(), r.result_bytes));
    let state = if closed { "closed" } else { state };
    format!(
        "@{sid} rev={rev} state={state} agent={agent} attention={attention} result_bytes={result_bytes}"
    )
}

/// The `status`/`sessions` tokens: `task_state=<state|-> task_rev=<n>`
/// (`task=` is the Land face's, printed before `pause=`).
#[must_use]
pub(crate) fn status_tokens(record: Option<&TaskRecord>) -> String {
    match record {
        Some(r) => format!("task_state={} task_rev={}", r.state.as_str(), r.rev),
        None => "task_state=- task_rev=0".to_string(),
    }
}

#[cfg(test)]
#[path = "task_record_tests.rs"]
mod tests;
