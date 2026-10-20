// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER'S RESULT (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
//! §2.3): what a Claude Code worker reported when its task ended, read from
//! the conversation its process writes — the transcript the harness already
//! reads for the model, the live upgrade's READY and the limit walls
//! ([`super::footer`], [`super::upgrade`]) — and snapshotted ONCE, at the
//! done edge, into `<state>/tasks/<gsid>-<rev>.result.md` ([`snapshot`]).
//! A turn typed into a done worker (the live upgrade's READY, a person's
//! question) cannot change it.
//!
//! The done edge is the SCREEN's, and Claude Code writes the turn's last rows
//! a moment after its screen shows the turn over (MEASURED, 2.1.301 on a real
//! login, 2026-10-15: three of five done edges read the transcript before its
//! final assistant row, which lands about 20 ms before the turn's
//! `turn_duration` row). So the snapshot waits for the end row of THE TURN
//! the done was read on — the newest begun by a row stamped no later than
//! the harness's read that decided the done: a prompt, or a row of Claude
//! Code's own that started a turn once the one before had ended (a
//! background command's `<task-notification>` that woke the worker after it
//! ended a turn to wait, an `isMeta` row Claude Code starts a turn on
//! itself) — so a notification written after that read starts no turn
//! waited for, while the wake whose end the done is, its final reply not
//! written yet, is waited for ([`settled`]: read again every
//! [`SETTLE_EVERY`], at most [`SETTLE_WITHIN`]) — and is read from the
//! transcript as it then stands, up to that turn's end: its end row, or the
//! next turn's start, a turn that wrote no end row included; past the
//! bound it is read as it is, its miss said as ever. A miss of that kind
//! (`no-reply`, `no-marker`, by either rule) keeps that turn's user row in
//! its sidecar, and while the miss stands each `result` reads THAT turn
//! again, once ([`reread`]) — never a later one; a reply found there becomes
//! the result.
//!
//! Which rule ended the task is the turn-end policy's word ([`DoneBy`]: the
//! act its done point followed), never this reader's guess — save that a
//! newest turn the done check opened is read by the check's rule whatever
//! the policy says: a loop that starts at a point another loop's check
//! answered (another supervisor's hand-back, the harness's fault restart, a
//! seamless update's successor) follows no act of its own
//! there, and its policy calls the marker that answered the check the
//! worker's own:
//!
//! * DONE THROUGH THE DONE CHECK — the conversation's newest turn is the one
//!   the harness's done check opened, as it types it (with the standing
//!   rules or without, [`is_done_check`]): the result is the worker's
//!   REPORT, the final assistant message of the turn BEFORE that check,
//!   then what the reply that answered the check says beyond its done, read
//!   as the marker's rule reads a turn: its final message, marker-only lines
//!   dropped, and where that is a bare done (the marker, `DONE`) the turn's
//!   last earlier message with words. The check asks for "finished and
//!   verified", and a worker that verifies or fixes inside the check's turn
//!   writes its final word there (a report, a commit, a bare marker
//!   included): the fact line every task carries promises the final message
//!   before the marker, and the report before it keeps a short "Verified."
//!   from standing alone. An earlier check answered with a done
//!   ([`says_done`]: the reply the policy ends the task on, which a loop
//!   started again asks again) is passed over for the turn before it, its
//!   words beyond the done kept in order. The policy ends a task on a check
//!   only where the reply says done, so a check turn whose final text is no
//!   done — none yet, or a preamble before its tool calls, the reply not
//!   written — is `no-reply`, the lag [`reread`] reads again.
//! * DONE BY THE MARKER — the worker ended its own turn with
//!   [`FINISHED`](crate::supervise::policy::turn_end::FINISHED): the final
//!   assistant message of that newest turn, which must end with the marker
//!   on a line of its own, its marker-only lines dropped ([`without_finished_lines`],
//!   the survey's `said` too). A final message that is the marker alone (a
//!   report, a check of the work, then the marker) gives the turn's last
//!   earlier message with words.
//!
//! A turn the live upgrade's announcement opened ([`ANNOUNCE_HEAD`]: its
//! READY, typed by the harness into a done worker) is the harness's and no
//! work of the worker's: the two rules read past it.
//!
//! The rows are read as the upgrade reads them: a subagent's
//! (`isSidechain`), Claude Code's own (a `<synthetic>` model,
//! `isApiErrorMessage`), `isMeta` rows and compaction summaries are no one's
//! words, and an assistant message written over several rows (Claude Code
//! writes one per content block) is ONE message, merged by its `message.id`.
//! A user row opens a turn when it holds typed words — a person's, a
//! manager's, the harness's own `keep going` and done check; tool results and
//! Claude Code's tagged rows (`<command-name>`, `<local-command-stdout>`,
//! `<bash-input>`, …) do not.
//!
//! What a snapshot came from stands BESIDE it, never inside the text: the
//! sidecar's one line (`<gsid>-<rev>.result.src`, [`Source`]) says
//! `source=transcript session=<id> uuid=<row>` and which transcript, or why
//! there is no result. A shape this reader does not know is that word
//! (`why=no-marker`, …), never a screen read that looks like an answer.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::upgrade::ANNOUNCE_HEAD;
use crate::supervise::policy::turn_end::{
    is_done_check, says_done, says_finished, without_finished_lines,
};

/// The most of a result one `result` reply prints, in characters; the rest
/// is in the file its last line names. Claude Code's Bash tool shows about
/// 30,000 characters of a command's output.
pub const RESULT_MAX_CHARS: usize = 24_000;

/// The most of a transcript the result is read from, from its end — at the
/// done edge ([`snapshot`]'s tail) and by `result full`: the footer's
/// back-read bound. Not the live upgrade's 256 KiB tail: a reply to the done
/// check that verifies again (the check asks for "finished and verified")
/// carries the suites' output in its tool rows, and the check's own prompt
/// and the report before it must both be read.
pub const TRANSCRIPT_READ_BYTES: u64 = 16 * 1024 * 1024;

/// The most of a snapshot `result` reads.
const RESULT_READ_BYTES: u64 = 16 * 1024 * 1024;

/// The most of a sidecar `result` reads: one line.
const SIDECAR_READ_BYTES: u64 = 8 * 1024;

/// The longest the done edge waits for its turn's end row ([`settled`]): a
/// DEADLINE, not a latency — the wait ends at the read that holds the row,
/// which Claude Code writes milliseconds after its screen shows the turn
/// over. Past it the transcript is read as it stands.
pub const SETTLE_WITHIN: Duration = Duration::from_secs(5);

/// How often the done edge reads the transcript's end again while it waits.
pub const SETTLE_EVERY: Duration = Duration::from_millis(150);

/// How much of the transcript's end each read again takes while it waits:
/// the newest turn's end row, and its prompt where the turn is short. A turn
/// longer than this has its prompt before the read, and its end row in it.
const END_READ_BYTES: u64 = 256 * 1024;

/// The misses a reply Claude Code had not written yet explains: either rule
/// found the newest turn's reply missing or unfinished ([`reread`]).
const LAG_MISSES: [&str; 2] = ["no-reply", "no-marker"];

/// A worker's report, as its transcript holds it ([`report`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The message's text blocks in order, a blank line between two, trimmed.
    pub text: String,
    /// The conversation's id (`sessionId`), `-` where its row names none.
    pub session: String,
    /// The id of the row that carries the message's last text block.
    pub uuid: String,
}

/// One row of a conversation as this reader takes it.
#[derive(Debug)]
enum Item {
    /// A turn's start: the words typed (a paste read as what was pasted).
    Prompt(String),
    /// One assistant message, its rows merged by `message.id`.
    Said {
        id: Option<String>,
        texts: Vec<String>,
        uuid: String,
        session: String,
    },
}

impl Item {
    fn is_prompt(&self) -> bool {
        matches!(self, Item::Prompt(_))
    }

    /// The message's text, when it has any: its blocks, a blank line apart.
    fn said(&self) -> Option<Report> {
        let Item::Said {
            texts,
            uuid,
            session,
            ..
        } = self
        else {
            return None;
        };
        let text = texts.join("\n\n").trim().to_string();
        (!text.is_empty()).then(|| Report {
            text,
            session: session.clone(),
            uuid: uuid.clone(),
        })
    }
}

/// The text parts of a `message.content`: the whole content when it is a
/// string, else every `text` block's text.
fn texts_of(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// One transcript `line` as this reader parses it; `None` for a row that is
/// no one's words — a subagent's (`isSidechain`), an `isMeta` row, a
/// compaction summary, Claude Code's own API error — and for a line that is
/// not JSON (a tail's cut first line, a half-written last one).
fn row_of(line: &str) -> Option<Value> {
    let v = super::transcript::exchange(line).ok()?;
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isSidechain")
        || flag("isMeta")
        || flag("isCompactSummary")
        || flag("isApiErrorMessage")
    {
        return None;
    }
    Some(v)
}

/// A user row's words: its text (a paste read as what was pasted), Claude
/// Code's tagged rows (`<task-notification>`, `<command-name>`, …) as they
/// stand; `None` for a row with none (a tool's result).
fn words_of(message: &Value) -> Option<String> {
    let typed = texts_of(message.get("content")).join("\n");
    let words = super::upgrade::typed_words(&typed);
    (!words.is_empty()).then(|| words.to_owned())
}

/// What one transcript `line` is to this reader; `None` for every row that
/// is no one's words or opens no turn, and for a line that is not JSON (a
/// tail's cut first line, a half-written last one).
fn item_of(line: &str) -> Option<Item> {
    if !line.contains("\"user\"") && !line.contains("assistant") {
        return None;
    }
    let v = row_of(line)?;
    let message = v.get("message")?;
    let word = |key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string()
    };
    match v.get("type").and_then(Value::as_str)? {
        "assistant" => {
            // Claude Code's own rows name no model of the API's.
            if message
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|m| !super::upgrade::is_model_id(m))
            {
                return None;
            }
            Some(Item::Said {
                id: message.get("id").and_then(Value::as_str).map(str::to_owned),
                texts: texts_of(message.get("content")),
                uuid: word("uuid"),
                session: word("sessionId"),
            })
        }
        "user" => {
            let words = words_of(message)?;
            (!words.starts_with('<')).then_some(Item::Prompt(words))
        }
        _ => None,
    }
}

/// The conversation `jsonl` holds, in order: its prompts, and its assistant
/// messages with each message's later rows merged into its first. A row of a
/// message already read in the same turn joins it; its id names the row
/// with the message's last text.
fn items(jsonl: &str) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for line in jsonl.lines() {
        let Some(item) = item_of(line) else {
            continue;
        };
        if let Item::Said {
            id: Some(id),
            texts,
            uuid,
            ..
        } = &item
        {
            let earlier = out
                .iter_mut()
                .rev()
                .take_while(|i| !i.is_prompt())
                .find_map(|i| match i {
                    Item::Said {
                        id: Some(seen),
                        texts: kept,
                        uuid: kept_uuid,
                        ..
                    } if seen == id => Some((kept, kept_uuid)),
                    _ => None,
                });
            if let Some((kept, kept_uuid)) = earlier {
                if !texts.is_empty() {
                    kept.extend(texts.iter().cloned());
                    kept_uuid.clone_from(uuid);
                }
                continue;
            }
        }
        out.push(item);
    }
    out
}

/// The last message of `items` that has text.
fn final_said(items: &[Item]) -> Option<Report> {
    items.iter().rev().find_map(Item::said)
}

/// One turn of a conversation: the words that opened it (`None` for the rows
/// before a tail's first prompt) and what was said in it.
struct TurnRows<'a> {
    prompt: Option<&'a str>,
    said: &'a [Item],
}

impl TurnRows<'_> {
    /// Whether the harness's done check opened this turn.
    fn is_check(&self) -> bool {
        self.prompt.is_some_and(is_done_check)
    }

    /// Whether what this turn said is a done ([`says_done`]: the marker,
    /// `DONE`, a done phrase — what the policy reads a check's answer by),
    /// or nothing.
    fn answered_done(&self) -> bool {
        final_said(self.said).is_none_or(|r| says_done(Some(&r.text)))
    }
}

/// `items` cut into turns, in order, the ones the live upgrade's
/// announcement opened ([`ANNOUNCE_HEAD`]) left out: the worker's turns.
fn worker_turns(items: &[Item]) -> Vec<TurnRows<'_>> {
    let mut out = Vec::new();
    let mut prompt = None;
    let mut start = 0;
    for (at, item) in items.iter().enumerate() {
        if let Item::Prompt(words) = item {
            if at > start || prompt.is_some() {
                out.push(TurnRows {
                    prompt,
                    said: &items[start..at],
                });
            }
            prompt = Some(words.as_str());
            start = at + 1;
        }
    }
    out.push(TurnRows {
        prompt,
        said: &items[start..],
    });
    out.retain(|t| !t.prompt.is_some_and(|p| p.starts_with(ANNOUNCE_HEAD)));
    out
}

/// Which of the turn-end policy's rules ended the task: what its done point
/// followed (`TurnEndState::followed`), handed to [`report`] by the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneBy {
    /// The worker said it was done after the harness's done check.
    Check,
    /// The worker ended a turn with
    /// [`FINISHED`](crate::supervise::policy::turn_end::FINISHED).
    Marker,
}

impl DoneBy {
    /// The rule's word on the record's mark (`why=`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            DoneBy::Check => "done-check",
            DoneBy::Marker => "marker",
        }
    }
}

/// THE WORKER'S REPORT in the transcript tail `jsonl`, read at the done edge
/// by [the module's](self) rule `by` — the check's wherever the newest turn
/// is a check. `Err` is the word for a shape this reader does not know:
/// `no-check` (done through the check, but the newest turn is no check),
/// `no-report` (nothing before the check said anything, and its reply
/// added no words), `no-reply` (the newest turn has no answer with text
/// yet; by the check's rule, no final text that says done), `no-marker`
/// (its answer does not end with the marker) or `empty` (the marker was all
/// the turn said).
///
/// # Errors
/// The shape's word, as above.
pub fn report(jsonl: &str, by: DoneBy) -> Result<Report, &'static str> {
    read(jsonl, by).report
}

/// What [`read`] read.
struct Reading {
    /// The rule it read by: `by`, or [`DoneBy::Check`] where the newest turn
    /// is a check.
    by: DoneBy,
    /// [`report`]'s answer.
    report: Result<Report, &'static str>,
    /// The user row that started the done edge's turn — the newest in the
    /// settled tail, which stops at that turn's end ([`Opener::of`]: a
    /// prompt, or a `<task-notification>` that woke the worker) — where
    /// either rule found the newest turn's reply missing or unfinished
    /// ([`LAG_MISSES`]): a reply Claude Code may not have written yet, which
    /// [`reread`] reads again for that turn alone.
    turn: Option<String>,
}

/// [`report`], the rule it read by and the turn a lag miss names
/// ([`Reading`]).
fn read(jsonl: &str, by: DoneBy) -> Reading {
    let items = items(jsonl);
    let turns = worker_turns(&items);
    let Some((newest, before)) = turns.split_last() else {
        return Reading {
            by,
            report: Err("no-reply"),
            turn: None,
        };
    };
    let (by, report) = if by == DoneBy::Check || newest.is_check() {
        (DoneBy::Check, by_check(newest, before))
    } else {
        (DoneBy::Marker, by_marker(newest))
    };
    let lagged = report.as_ref().is_err_and(|why| LAG_MISSES.contains(why));
    Reading {
        by,
        report,
        turn: match Opener::of(jsonl, None) {
            Opener::Row(uuid) if lagged => Some(uuid),
            _ => None,
        },
    }
}

/// DONE THROUGH THE DONE CHECK, `newest` the turn the check opened and
/// `before` the turns before it ([the module's](self) rule): the REPORT —
/// the final message of the turn before the check, earlier checks answered
/// with a done passed over — then what each passed-over check's reply and
/// `newest`'s say beyond the done ([`check_words`]), oldest first, a blank
/// line apart. So a worker that verified or fixed inside the check's turn
/// has its final word there returned too. The report names the row of its
/// last text. `no-check`: `newest` is no check; `no-reply`: `newest`'s
/// final text is no done ([`says_done`]) — none yet, or a preamble before
/// its tool calls — where the policy ends a task on a check only on one, so
/// its reply is one Claude Code had not written, which [`reread`] reads
/// again; `no-report`: nothing before the check said anything and no reply
/// added words.
fn by_check(newest: &TurnRows<'_>, before: &[TurnRows<'_>]) -> Result<Report, &'static str> {
    if !newest.is_check() {
        return Err("no-check");
    }
    if !final_said(newest.said).is_some_and(|r| says_done(Some(&r.text))) {
        return Err("no-reply");
    }
    let mut checks = vec![newest];
    let mut earlier = before.iter().rev();
    let reported = loop {
        match earlier.next() {
            Some(t) if t.is_check() && t.answered_done() => checks.push(t),
            other => break other,
        }
    };
    let parts: Vec<Report> = reported
        .and_then(|t| final_said(t.said))
        .into_iter()
        .chain(checks.iter().rev().filter_map(|t| check_words(t)))
        .collect();
    let last = parts.last().ok_or("no-report")?;
    Ok(Report {
        text: parts
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        session: last.session.clone(),
        uuid: last.uuid.clone(),
    })
}

/// What a check turn's reply says beyond the done it answered with, as the
/// marker's rule reads a turn ([`by_marker`]): its last message with words
/// once marker-only lines are dropped ([`without_finished_lines`]) and a
/// bare `DONE` (the policy's other done word) is read as none — its final
/// message, or where that is a bare done, the last earlier one with words
/// (a report, then a commit, then the marker alone). `None` where no message
/// of the turn has any.
fn check_words(turn: &TurnRows<'_>) -> Option<Report> {
    turn.said
        .iter()
        .rev()
        .filter_map(Item::said)
        .find_map(|said| {
            let text = without_finished_lines(&said.text);
            let word = text.trim_matches(|c: char| !c.is_alphanumeric());
            (!(word.is_empty() || word == "DONE")).then_some(Report { text, ..said })
        })
}

/// What one transcript line is to a turn's end ([`end_of`]).
enum Mark {
    /// A main-chain user row with words — any of which can start Claude
    /// Code on a turn: a prompt [`report`] reads (`prompt`), or one it
    /// reads past — a background command's `<task-notification>`, which
    /// wakes a manager that ended its turn to wait, a slash command, the
    /// `isMeta` rows Claude Code starts a turn on itself (a peer session's
    /// message, its continuation after a usage limit resets; `meta`) — the
    /// row's id, and when it was written (`at`, unix milliseconds).
    Opens {
        uuid: Option<String>,
        prompt: bool,
        meta: bool,
        at: Option<i64>,
    },
    /// A main-chain assistant row: the turn has run.
    Said,
    /// Claude Code's own main-chain `turn_duration` row, which it writes as a
    /// turn ends, after the turn's last reply row.
    End,
}

impl Mark {
    /// Whether this row, read after a turn's own row, ENDS that turn —
    /// `said`: an assistant row of the turn came before it. A prompt does,
    /// and so does a tagged row that is no `isMeta` one (a background
    /// command's `<task-notification>`, a slash command's rows) once the turn
    /// has said something: Claude Code writes those only at its prompt, after
    /// a turn is over (MEASURED over this machine's seven transcripts,
    /// 2026-10-16: every one of 816 `<task-notification>` rows and 84 other
    /// tagged rows follows a `turn_duration` row, another tagged row or the
    /// away summary, never an assistant row), so one after the turn's words
    /// is the next turn's start in a turn that wrote no end row (an
    /// interrupted one). An `isMeta` row is not: Claude Code writes those
    /// inside a running turn too (a skill's body, a hook's message).
    fn ends_turn(&self, said: bool) -> bool {
        match self {
            Mark::End => true,
            Mark::Opens { prompt: true, .. } => true,
            Mark::Opens { meta: false, .. } => said,
            Mark::Opens { .. } | Mark::Said => false,
        }
    }
}

/// What `line` is to a turn's end; `None` for every other row, a
/// subagent's rows and a row still half written among them.
fn mark_of(line: &str) -> Option<Mark> {
    if line.contains("turn_duration") && is_turn_end(line) {
        return Some(Mark::End);
    }
    if !line.contains("\"user\"") && !line.contains("\"assistant\"") {
        return None;
    }
    let v = super::transcript::exchange(line).ok()?;
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isSidechain") {
        return None;
    }
    match v.get("type").and_then(Value::as_str)? {
        "assistant" => Some(Mark::Said),
        "user" => {
            let words = words_of(v.get("message")?)?;
            let meta = flag("isMeta") || flag("isCompactSummary");
            Some(Mark::Opens {
                uuid: v.get("uuid").and_then(Value::as_str).map(str::to_owned),
                prompt: !(meta || words.starts_with('<')),
                meta,
                at: v
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(stamp_ms),
            })
        }
        _ => None,
    }
}

/// A row's `timestamp` as Claude Code writes it (`2026-10-15T17:28:25.422Z`,
/// UTC to the millisecond) in unix milliseconds; `None` for any other shape.
fn stamp_ms(stamp: &str) -> Option<i64> {
    let secs = aterm_types::rfc3339::parse_utc_fractional(stamp)?;
    // Checked by the parse: `.` and one to nine digits before the `Z`, or
    // nothing.
    let digits = stamp.get(20..stamp.len() - 1).unwrap_or("");
    let ms = digits
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0, |n, d| n * 10 + i64::from(d - b'0'));
    secs.checked_mul(1000)?.checked_add(ms)
}

/// Whether `line` can be a [`Mark::Opens`] or a [`Mark::End`] — a user row
/// or an end row — read cheaply: once a turn has said something, its other
/// assistant rows tell nothing more, and are not parsed.
fn may_open_or_end(line: &str) -> bool {
    line.contains("\"user\"") || line.contains("turn_duration")
}

/// Whether `line` is a main-chain `turn_duration` row.
fn is_turn_end(line: &str) -> bool {
    let Ok(v) = super::transcript::usage(line) else {
        return false;
    };
    v.get("type").and_then(Value::as_str) == Some("system")
        && v.get("subtype").and_then(Value::as_str) == Some("turn_duration")
        && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
}

/// THE DONE EDGE'S TURN, as its first read of the transcript places it
/// ([`Opener::of`]). Every later read is judged for THAT turn, never for one
/// begun since.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Opener {
    /// The row that started it, by its id.
    Row(String),
    /// The read holds no such row: a tail inside one long turn, which every
    /// later read's prompt follows.
    Before,
    /// Its row names no id (Claude Code's always do): each read's newest.
    Newest,
}

impl Opener {
    /// The done edge's turn in its first read, `jsonl`: THE NEWEST TURN
    /// THE DONE WAS READ ON — begun by a row stamped no later than `shown`,
    /// the harness's read that decided the done (unix milliseconds; `None`,
    /// or a row with no stamp: no row is later). A turn starts at a prompt,
    /// and at a row of Claude Code's own once the turn before has ended
    /// ([`Mark::Opens`]: a background command's `<task-notification>` that
    /// woke the worker after a turn it ended to wait, the continuation after
    /// a usage limit resets) or has spoken ([`Mark::ends_turn`]). So a
    /// notification written after that read (a background shell the worker
    /// left, ended at once) starts no turn the snapshot waits for or takes,
    /// and neither does a prompt typed after it; while a turn a notification
    /// began before it — the wake whose end IS the done, after a marker held
    /// back over a background agent — is the one waited for, its final reply
    /// not written yet or not. A row stamped between the screen drawing the
    /// done and the read is read as the done's: that window is the loop's
    /// read of the point (up to the policy's `take_within`, 30 s, where no
    /// read saw the turn run), and for a loop that started at a done point,
    /// the whole time no loop read it. Rows are written in time order, so the
    /// first start stamped later ends the look. A read that holds no turn's
    /// start is [`Opener::Before`].
    fn of(jsonl: &str, shown: Option<i64>) -> Self {
        let mut newest = Opener::Before;
        let mut ended = false;
        let mut said = false;
        for line in jsonl.lines() {
            if said && !may_open_or_end(line) {
                continue;
            }
            match mark_of(line) {
                Some(Mark::End) => ended = true,
                Some(Mark::Said) => said = true,
                Some(Mark::Opens {
                    uuid,
                    prompt,
                    meta,
                    at,
                }) if prompt || ended || (said && !meta) => {
                    if shown.zip(at).is_some_and(|(shown, at)| at > shown) {
                        break;
                    }
                    newest = uuid.map_or(Opener::Newest, Opener::Row);
                    (ended, said) = (false, false);
                }
                // An `isMeta` row inside the turn (a skill's body), or a row
                // of the same start: the turn's own.
                Some(Mark::Opens { .. }) | None => {}
            }
        }
        newest
    }
}

/// Where a turn ends in a read of the transcript ([`end_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct End {
    /// Whether it has ended: a `turn_duration` row after its row, or the
    /// next turn's start ([`Mark::ends_turn`]): a later prompt — one typed
    /// after the turn was over, at its prompt again — or a tagged row after
    /// the turn's words.
    ended: bool,
    /// Where its rows stop in the read (a line's start: just past its end
    /// row, at the next turn's row, else the read's end); `None` where the
    /// read does not hold its row (it began after it), so nothing is cut.
    upto: Option<usize>,
}

/// Where THE TURN `opener` started ends in the transcript read `jsonl`:
/// after its row, the first `turn_duration` row or next turn's start
/// decides ([`End`], [`Mark::ends_turn`]). An `isMeta` row in between (a
/// skill's body) is the same turn's, and so is a tagged row before the turn
/// said anything. A read that does not hold the row is read from its start:
/// it began inside the turn or after it.
fn end_of(jsonl: &str, opener: &Opener) -> End {
    let mut at = 0;
    let mut from = None;
    for line in jsonl.split_inclusive('\n') {
        let next = at + line.len();
        let is_it = match opener {
            Opener::Row(uuid) => {
                line.contains(uuid.as_str())
                    && matches!(mark_of(line), Some(Mark::Opens { uuid: Some(row), .. }) if row == *uuid)
            }
            Opener::Newest => matches!(mark_of(line), Some(Mark::Opens { .. })),
            Opener::Before => false,
        };
        if is_it {
            from = Some(next);
            if matches!(opener, Opener::Row(_)) {
                break;
            }
        }
        at = next;
    }
    let placed = from.is_some() || *opener == Opener::Before;
    let mut at = from.unwrap_or(0);
    let mut said = false;
    for line in jsonl[at..].split_inclusive('\n') {
        let next = at + line.len();
        if said && !may_open_or_end(line) {
            at = next;
            continue;
        }
        match mark_of(line) {
            Some(Mark::End) => {
                return End {
                    ended: true,
                    upto: placed.then_some(next),
                };
            }
            Some(Mark::Said) => said = true,
            Some(mark) if mark.ends_turn(said) => {
                return End {
                    ended: true,
                    upto: placed.then_some(at),
                };
            }
            _ => {}
        }
        at = next;
    }
    End {
        ended: false,
        upto: placed.then_some(jsonl.len()),
    }
}

/// `jsonl` up to `end` ([`End::upto`]): the read with every row after the
/// turn's end left out; whole where the read does not place the turn.
fn upto(jsonl: String, end: End) -> String {
    match end.upto {
        Some(at) if at < jsonl.len() => jsonl[..at].to_string(),
        _ => jsonl,
    }
}

/// The done edge's transcript once it settled ([`settled`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// The tail to snapshot from, up to the done edge's turn's end (a turn
    /// begun after it left out): read when that turn's end row was there,
    /// else the freshest read (past the bound, or the file no longer read),
    /// else the first.
    pub tail: String,
    /// The pauses waited before that ([`SETTLE_EVERY`] each): `0` where the
    /// first read, or the read right after it, held the end row, or the file
    /// could not be read again.
    pub waits: u32,
    /// Whether the done edge's turn was read to its end: its end row, or a
    /// prompt typed after it.
    pub ended: bool,
}

impl Settled {
    /// What the wait came to, for the loop's journal: `the transcript's turn
    /// end was read after <n> waits`, or `no turn end in the transcript after
    /// <n> waits: read as it stands` (`1 wait` for one).
    #[must_use]
    pub fn note(&self) -> String {
        let waits = match self.waits {
            1 => "1 wait".to_string(),
            n => format!("{n} waits"),
        };
        if self.ended {
            format!("the transcript's turn end was read after {waits}")
        } else {
            format!("no turn end in the transcript after {waits}: read as it stands")
        }
    }
}

/// [`settled`] over an injected transcript and clock: `read(bytes)` gives
/// the transcript's last `bytes` as they stand now (`None`: it cannot be
/// read), `pause()` waits one step before the next read and is `false` once
/// the bound is spent. `first` is the done edge's own read, and the turn it
/// places for the read that decided the done, `shown` ([`Opener::of`]: the
/// newest begun by a row stamped no later), is THE TURN waited for — a
/// prompt typed during the wait is a later turn's, never this one's, and so
/// is one a row written after that read began. Where that
/// turn has ended in `first`, it is the tail, and nothing is read again;
/// else the end is read again at once and after each pause until it holds
/// the turn's end row (or a later prompt), and the tail is then read whole
/// ([`TRANSCRIPT_READ_BYTES`]) — past the bound too, so a reply written
/// without its end row is still read. Either way the tail stops at that
/// turn's end, so the snapshot reads that turn, and its
/// sidecar names it. A transcript that cannot be read again leaves `first`.
pub fn settle_with(
    first: String,
    shown: Option<i64>,
    mut read: impl FnMut(u64) -> Option<String>,
    mut pause: impl FnMut() -> bool,
) -> Settled {
    let opener = Opener::of(&first, shown);
    let end = end_of(&first, &opener);
    if end.ended {
        return Settled {
            tail: upto(first, end),
            waits: 0,
            ended: true,
        };
    }
    let mut waits = 0;
    let ended = loop {
        match read(END_READ_BYTES) {
            None if waits == 0 => {
                return Settled {
                    tail: first,
                    waits,
                    ended: false,
                };
            }
            None => break false,
            Some(tail) if end_of(&tail, &opener).ended => break true,
            Some(_) => {}
        }
        if !pause() {
            break false;
        }
        waits += 1;
    };
    let tail = match read(TRANSCRIPT_READ_BYTES) {
        Some(whole) => {
            let end = end_of(&whole, &opener);
            upto(whole, end)
        }
        None => first,
    };
    Settled { tail, waits, ended }
}

/// THE DONE EDGE'S TRANSCRIPT, SETTLED: `first`, the tail of the transcript
/// at `path` the host read at the screen's done point — the done decided by
/// the loop's read at `shown` (unix milliseconds) — where the turn the done
/// was read on has ended ([`settle_with`]); else the transcript read again every
/// [`SETTLE_EVERY`] until THAT turn's end row is there, at most
/// [`SETTLE_WITHIN`] in all and not past `stopping()` (the loop's stop),
/// then read whole, up to that turn's end ([`settle_with`]). It takes no
/// lock, and holds the calling loop no longer than the bound.
pub fn settled(
    path: &Path,
    first: String,
    shown: Option<i64>,
    stopping: impl Fn() -> bool,
) -> Settled {
    let deadline = Instant::now() + SETTLE_WITHIN;
    settle_with(
        first,
        shown,
        |bytes| tail_of(path, bytes).map(|(tail, _)| tail),
        || {
            let now = Instant::now();
            if now >= deadline || stopping() {
                return false;
            }
            std::thread::sleep(SETTLE_EVERY.min(deadline - now));
            true
        },
    )
}

/// The marker's rule on the newest turn ([`report`]).
fn by_marker(newest: &TurnRows<'_>) -> Result<Report, &'static str> {
    let said = final_said(newest.said).ok_or("no-reply")?;
    if !says_finished(Some(&said.text)) {
        return Err("no-marker");
    }
    std::iter::once(said)
        .chain(newest.said.iter().rev().filter_map(Item::said).skip(1))
        .find_map(|r| {
            let text = without_finished_lines(&r.text);
            (!text.is_empty()).then_some(Report { text, ..r })
        })
        .ok_or("empty")
}

/// THE WHOLE EXCHANGE since the task's user row — the first prompt in
/// `jsonl` — for `result full`: each prompt as `> <text>` (the harness's own
/// `keep going` and done check among them), each assistant message's text,
/// a blank line between two; and whether it began at a prompt. A `jsonl`
/// that holds no prompt (a tail that began inside the task's turns) gives
/// all it holds.
#[must_use]
pub fn thread(jsonl: &str) -> (String, bool) {
    let items = items(jsonl);
    let start = items.iter().position(Item::is_prompt);
    let mut out = String::new();
    for item in &items[start.unwrap_or(0)..] {
        let block = match item {
            Item::Prompt(t) => t
                .lines()
                .map(|l| {
                    if l.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {l}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            said => match said.said() {
                Some(r) => r.text,
                None => continue,
            },
        };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&block);
    }
    (out, start.is_some())
}

/// Where a snapshot came from, as its sidecar's one line says it
/// ([`snapshot`], [`answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The transcript: the conversation, the row the report is, and the file
    /// it was read from.
    Transcript {
        session: String,
        uuid: String,
        transcript: Option<PathBuf>,
    },
    /// No result, and why: [`report`]'s word, or `no-transcript`; and, for a
    /// reply that may yet be written ([`LAG_MISSES`]), the user row that
    /// started the done edge's turn ([`reread`]).
    Unavailable {
        why: String,
        transcript: Option<PathBuf>,
        turn: Option<String>,
    },
}

impl Source {
    /// The sidecar line: `source=transcript session=<pct> uuid=<pct>` or
    /// `source=none why=<word>` (and ` turn=<pct>` where it names the turn),
    /// then ` transcript=<pct>` when one was read.
    #[must_use]
    pub fn line(&self) -> String {
        let pct = aterm_codec::pct::encode;
        let (mut out, transcript) = match self {
            Source::Transcript {
                session,
                uuid,
                transcript,
            } => (
                format!(
                    "source=transcript session={} uuid={}",
                    pct(session),
                    pct(uuid)
                ),
                transcript,
            ),
            Source::Unavailable {
                why,
                transcript,
                turn,
            } => {
                let mut line = format!("source=none why={}", pct(why));
                if let Some(turn) = turn {
                    line.push_str(" turn=");
                    line.push_str(&pct(turn));
                }
                (line, transcript)
            }
        };
        if let Some(path) = transcript {
            out.push_str(" transcript=");
            out.push_str(&pct(&path.to_string_lossy()));
        }
        out
    }

    /// [`Self::line`] read back; `None` for a line it never writes.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
                .map(aterm_codec::pct::decode)
        };
        let transcript = field("transcript").map(PathBuf::from);
        match field("source")?.as_str() {
            "transcript" => Some(Source::Transcript {
                session: field("session")?,
                uuid: field("uuid")?,
                transcript,
            }),
            "none" => Some(Source::Unavailable {
                why: field("why").filter(|w| is_word(w))?,
                transcript,
                turn: field("turn"),
            }),
            _ => None,
        }
    }
}

/// A why word: `[a-z0-9-]`, 1 to 32 bytes.
fn is_word(w: &str) -> bool {
    (1..=32).contains(&w.len())
        && w.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The sidecar beside the snapshot at `path`: `<stem>.result.src`.
#[must_use]
pub fn sidecar_of(path: &Path) -> PathBuf {
    path.with_extension("src")
}

/// What [`snapshot`] wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The result's file, `<tasks>/<stem>.result.md`.
    pub path: PathBuf,
    /// Its size; `0` where there is no result.
    pub bytes: u64,
    /// Why there is no result ([`report`]'s word, or `no-transcript`).
    pub unavailable: Option<&'static str>,
    /// The rule the result was read by: the one asked for, or the check's
    /// where the newest turn is a check ([`report`]) — the mark's `why=`.
    pub by: DoneBy,
}

/// SNAPSHOT THE RESULT, at the done edge: the report [`report`] reads by rule
/// `by` in `transcript` (the file it was read from and its tail; `None`: no
/// transcript the host could read), written to `<tasks>/<stem>.result.md`
/// and its source to the sidecar beside it ([`Source`]), each 0600 through a
/// rename, under a 0700 `tasks` made where missing. A shape the reader does
/// not know writes an empty result and its word. `stem` is one file name's
/// stem: `[A-Za-z0-9_-]`.
///
/// # Errors
/// A `stem` that is no such stem, or the directory or a file could not be
/// written.
pub fn snapshot(
    tasks: &Path,
    stem: &str,
    transcript: Option<(&Path, &str)>,
    by: DoneBy,
) -> std::io::Result<Snapshot> {
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{stem:?} is no result file's stem"),
        ));
    }
    let (by, got) = match transcript {
        None => (by, None),
        Some((file, jsonl)) => {
            let reading = read(jsonl, by);
            (reading.by, Some((file, reading.report, reading.turn)))
        }
    };
    let (text, source, unavailable) = match got {
        None => (
            String::new(),
            Source::Unavailable {
                why: "no-transcript".to_string(),
                transcript: None,
                turn: None,
            },
            Some("no-transcript"),
        ),
        Some((file, Ok(r), _)) => (
            r.text,
            Source::Transcript {
                session: r.session,
                uuid: r.uuid,
                transcript: Some(file.to_path_buf()),
            },
            None,
        ),
        Some((file, Err(why), turn)) => (
            String::new(),
            Source::Unavailable {
                why: why.to_string(),
                transcript: Some(file.to_path_buf()),
                turn,
            },
            Some(why),
        ),
    };
    if !tasks.is_dir() {
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            dir.mode(0o700);
        }
        dir.create(tasks)?;
    }
    let path = tasks.join(format!("{stem}.result.md"));
    // The text first: a sidecar never names a result that is not there.
    super::upgrade_models::write_atomic(&path, &text)?;
    super::upgrade_models::write_atomic(&sidecar_of(&path), &format!("{}\n", source.line()))?;
    Ok(Snapshot {
        path,
        bytes: text.len() as u64,
        unavailable,
        by,
    })
}

/// `ERR result unavailable why=<why>`.
fn unavailable(why: &str) -> String {
    format!("ERR result unavailable why={why}\n")
}

/// The reply: `OK <n> <stamp> bytes=<bytes><extra>` and the `n` lines.
fn framed(stamp: &str, bytes: usize, extra: &str, lines: &[String]) -> String {
    let mut out = format!("OK {} {stamp} bytes={bytes}{extra}\n", lines.len());
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `text` as reply lines, at most `max` characters of it, then — where it
/// was cut — `… truncated; full: <path>`.
fn head_lines(text: &str, max: usize, path: &Path) -> Vec<String> {
    let cut = text.char_indices().nth(max).map(|(at, _)| at);
    let shown = cut.map_or(text, |at| &text[..at]);
    let mut lines: Vec<String> = if shown.is_empty() {
        Vec::new()
    } else {
        shown
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect()
    };
    if cut.is_some() {
        lines.push(format!("… truncated; full: {}", path.display()));
    }
    lines
}

/// `text` as reply lines, its last `max` characters, led — where anything
/// before them was cut, or the read did not reach the file's start — by
/// `… earlier turns cut; the transcript: <path>`.
fn tail_lines(text: &str, max: usize, transcript: &Path, whole: bool) -> Vec<String> {
    let skip = text.chars().count().saturating_sub(max);
    let at = text
        .char_indices()
        .nth(skip)
        .map_or(text.len(), |(at, _)| at);
    let shown = &text[at..];
    let mut lines = Vec::new();
    if skip > 0 || !whole {
        lines.push(format!(
            "… earlier turns cut; the transcript: {}",
            transcript.display()
        ));
    }
    lines.extend(
        shown
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()),
    );
    lines
}

/// At most the last `bytes` of the regular file at `path`, and whether that
/// is the whole file.
fn tail_of(path: &Path, bytes: u64) -> Option<(String, bool)> {
    let mut f = super::footer::open_regular(path)?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf).ok()?;
    Some((String::from_utf8_lossy(&buf).into_owned(), start == 0))
}

/// THE `result [full]` REPLY over the snapshot at `path` ([`snapshot`]),
/// framed `OK <n>` + `n` lines:
///
/// * `result` — `OK <n> source=transcript session=<id> uuid=<row> bytes=<n>`,
///   then the result's text, at most [`RESULT_MAX_CHARS`] of it, then — where
///   it was cut — `… truncated; full: <path>`;
/// * `result full` — the whole exchange since the task's user row
///   ([`thread`]), read from the transcript NOW and bounded
///   ([`TRANSCRIPT_READ_BYTES`] read, its last [`RESULT_MAX_CHARS`] shown, led by
///   a line naming the transcript where earlier turns were cut), under the
///   same stamp and ` full=1`.
///
/// A snapshot that has no result answers `ERR result unavailable
/// why=<word>`: the word its sidecar keeps, `no-snapshot` with no sidecar,
/// `unreadable` for a file that cannot be read, `no-transcript` for a thread
/// with no transcript to read, `no-prompt` for one whose whole transcript
/// holds no prompt (a tail that began inside the task's turns is shown,
/// behind the line that says earlier turns are cut).
#[must_use]
pub fn answer(path: &Path, full: bool) -> String {
    match source_of(path) {
        None => unavailable("no-snapshot"),
        Some(Source::Unavailable { why, .. }) => unavailable(&why),
        Some(Source::Transcript {
            session,
            uuid,
            transcript,
        }) => reply(&session, &uuid, transcript.as_deref(), path, full, || {
            super::footer::read_small(path, RESULT_READ_BYTES)
        }),
    }
}

/// The sidecar beside the snapshot at `path`, read.
fn source_of(path: &Path) -> Option<Source> {
    super::footer::read_small(&sidecar_of(path), SIDECAR_READ_BYTES)
        .and_then(|t| Source::parse(t.lines().next()?))
}

/// [`answer`] over a result read from `transcript` (`session`, row `uuid`):
/// its `text()` (`None`: unreadable) for `result`, the thread read now for
/// `result full`.
fn reply(
    session: &str,
    uuid: &str,
    transcript: Option<&Path>,
    path: &Path,
    full: bool,
    text: impl FnOnce() -> Option<String>,
) -> String {
    let pct = aterm_codec::pct::encode;
    let stamp = format!(
        "source=transcript session={} uuid={}",
        pct(session),
        pct(uuid)
    );
    if !full {
        let Some(text) = text() else {
            return unavailable("unreadable");
        };
        return framed(
            &stamp,
            text.len(),
            "",
            &head_lines(&text, RESULT_MAX_CHARS, path),
        );
    }
    let Some(file) = transcript else {
        return unavailable("no-transcript");
    };
    let Some((jsonl, whole)) = tail_of(file, TRANSCRIPT_READ_BYTES) else {
        return unavailable("no-transcript");
    };
    let (thread, began) = thread(&jsonl);
    if whole && !began {
        return unavailable("no-prompt");
    }
    framed(
        &stamp,
        thread.len(),
        " full=1",
        &tail_lines(&thread, RESULT_MAX_CHARS, file, whole && began),
    )
}

/// A result the done edge missed, found when its turn was read again
/// ([`reread`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reread {
    /// The report, as its turn's rule reads it now.
    pub report: Report,
    /// The transcript it was read from.
    pub transcript: PathBuf,
}

impl Reread {
    /// The result's size: what the record's `result_bytes=` says.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.report.text.len() as u64
    }

    /// The sidecar's source for it.
    fn source(&self) -> Source {
        Source::Transcript {
            session: self.report.session.clone(),
            uuid: self.report.uuid.clone(),
            transcript: Some(self.transcript.clone()),
        }
    }

    /// The snapshot at `path` and its sidecar written over with it, each
    /// through a rename and the text first, as [`snapshot`] writes them.
    ///
    /// # Errors
    /// A file could not be written.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        super::upgrade_models::write_atomic(path, &self.report.text)?;
        super::upgrade_models::write_atomic(
            &sidecar_of(path),
            &format!("{}\n", self.source().line()),
        )
    }

    /// The `result [full]` reply for it ([`answer`]), whether or not it was
    /// written over the snapshot at `path`.
    #[must_use]
    pub fn answer(&self, path: &Path, full: bool) -> String {
        reply(
            &self.report.session,
            &self.report.uuid,
            Some(&self.transcript),
            path,
            full,
            || Some(self.report.text.clone()),
        )
    }
}

/// A RESULT THE DONE EDGE MISSED, READ AGAIN — the backstop to [`settled`]:
/// the snapshot at `path` whose sidecar says its turn's reply was missing or
/// unfinished (`why=no-reply|no-marker`) and names the user row that started
/// the done edge's turn (`turn=`: a prompt, the done check, or a
/// `<task-notification>`) and the transcript, read again from that
/// transcript's end ([`TRANSCRIPT_READ_BYTES`]) as the snapshot would have
/// read it once that turn ended: up to that turn's end row — or the next
/// prompt — by the rule that turn is read by (the check's where the check
/// opened it, else the marker's). So a later turn's reply
/// is never taken, a notification's that woke the worker after it ended
/// among them. `None`: no such miss, no transcript to read, the turn's row
/// not in what was read, or its reply still no report.
#[must_use]
pub fn reread(path: &Path) -> Option<Reread> {
    let Some(Source::Unavailable {
        why,
        transcript: Some(transcript),
        turn: Some(turn),
    }) = source_of(path)
    else {
        return None;
    };
    if !LAG_MISSES.contains(&why.as_str()) {
        return None;
    }
    let (jsonl, _) = tail_of(&transcript, TRANSCRIPT_READ_BYTES)?;
    let end = end_of(&jsonl, &Opener::Row(turn));
    end.upto?;
    let report = read(&upto(jsonl, end), DoneBy::Marker).report.ok()?;
    Some(Reread { report, transcript })
}

#[cfg(test)]
#[path = "result_tests.rs"]
mod tests;
