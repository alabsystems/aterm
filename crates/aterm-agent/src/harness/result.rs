// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER'S RESULT (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.3).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::upgrade::{ANNOUNCE_HEAD, is_land_act};
use crate::supervise::policy::turn_end::{is_done_check, says_done, without_finished_lines};

/// The most of a result one `result` reply prints, in characters; the rest is in the file its last line names.
pub const RESULT_MAX_CHARS: usize = 24_000;

/// The most of a transcript the result is read from, from its end.
pub const TRANSCRIPT_READ_BYTES: u64 = 16 * 1024 * 1024;

/// The most of a snapshot `result` reads.
const RESULT_READ_BYTES: u64 = 16 * 1024 * 1024;

/// The most of a sidecar `result` reads: one line.
const SIDECAR_READ_BYTES: u64 = 8 * 1024;

/// The longest the done edge waits for its turn's end row ([`settled`]): a DEADLINE, not a latency.
pub const SETTLE_WITHIN: Duration = Duration::from_secs(5);

/// How often the done edge reads the transcript's end again while it waits.
pub const SETTLE_EVERY: Duration = Duration::from_millis(150);

/// How much of the transcript's end each read again takes while it waits:
const END_READ_BYTES: u64 = 256 * 1024;

/// The misses a reply Claude Code had not written yet explains: either rule
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
        /// The message's last content block is a tool's call: the turn goes
        calls: Option<bool>,
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

/// Whether a `message.content`'s last block is a tool's call (`tool_use`);
fn last_block_calls(content: Option<&Value>) -> Option<bool> {
    match content {
        Some(Value::String(_)) => Some(false),
        Some(Value::Array(parts)) => parts
            .last()
            .map(|p| p.get("type").and_then(Value::as_str) == Some("tool_use")),
        _ => None,
    }
}

/// Whether the last assistant message of `items` ends in a tool's call
fn ends_in_call(items: &[Item]) -> bool {
    items.iter().rev().find_map(|i| match i {
        Item::Said { calls, .. } => Some(*calls),
        Item::Prompt(_) => None,
    }) == Some(Some(true))
}

/// The text parts of a `message.content`: the whole content when it is a
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
fn row_of(line: &str) -> Option<Value> {
    let v = super::transcript::exchange(line).ok()?;
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isSidechain")
        || flag("isMeta")
        || flag("isCompactSummary")
        || flag("queueTranscriptOnly")
        || flag("isApiErrorMessage")
    {
        return None;
    }
    Some(v)
}

/// A user row's words: its text (a paste read as what was pasted), Claude
fn words_of(message: &Value) -> Option<String> {
    let typed = texts_of(message.get("content")).join("\n");
    let words = super::upgrade::typed_words(&typed);
    (!words.is_empty()).then(|| words.to_owned())
}

/// What one transcript `line` is to this reader; `None` for every row that
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
                calls: last_block_calls(message.get("content")),
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
            calls,
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
                        calls: kept_calls,
                        ..
                    } if seen == id => Some((kept, kept_uuid, kept_calls)),
                    _ => None,
                });
            if let Some((kept, kept_uuid, kept_calls)) = earlier {
                if !texts.is_empty() {
                    kept.extend(texts.iter().cloned());
                    kept_uuid.clone_from(uuid);
                }
                if calls.is_some() {
                    *kept_calls = *calls;
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
struct TurnRows<'a> {
    prompt: Option<&'a str>,
    said: &'a [Item],
}

impl TurnRows<'_> {
    /// Whether a harness check opened this turn: a land act ([`Self::is_land`])
    fn is_check(&self) -> bool {
        self.is_land() || self.prompt.is_some_and(is_done_check)
    }

    /// Whether one of Land's acts opened this turn ([`is_land_act`]).
    fn is_land(&self) -> bool {
        self.prompt.is_some_and(is_land_act)
    }

    /// Whether what this turn said is a done ([`says_done`]: a defense, a
    fn answered_done(&self) -> bool {
        final_said(self.said).is_none_or(|r| says_done(Some(&r.text)))
    }
}

/// `items` cut into turns, in order, the ones the live upgrade's
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

/// The rule a result is read by ([the module's](self)): a check's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneBy {
    /// The newest turn is a harness check's: the report before it, then what its reply adds.
    Check,
    /// The newest turn's final message, no token required.
    Turn,
}

impl DoneBy {
    /// The rule's word, as a tape keeps it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            DoneBy::Check => "check",
            DoneBy::Turn => "turn",
        }
    }

    /// The rule a `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        match word {
            "check" | "done-check" => Some(DoneBy::Check),
            "turn" | "marker" => Some(DoneBy::Turn),
            _ => None,
        }
    }
}

/// THE WORKER'S REPORT in the transcript tail `jsonl`, read at the done edge by [the module's](self) rule `by`.
///
/// # Errors
/// The shape's word, as above.
pub fn report(jsonl: &str, by: DoneBy) -> Result<Report, &'static str> {
    read(jsonl, by).report
}

/// What [`read`] read.
struct Reading {
    /// The rule it read by: `by`, or [`DoneBy::Check`] where the newest turn
    by: DoneBy,
    /// [`report`]'s answer.
    report: Result<Report, &'static str>,
    /// The user row that started the done edge's turn — the newest in the
    turn: Option<String>,
}

/// [`report`], the rule it read by and the turn a lag miss names
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
        (DoneBy::Turn, by_turn(newest))
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

/// THE LANDING, `newest` the turn a harness check opened and `before` the
fn by_check(newest: &TurnRows<'_>, before: &[TurnRows<'_>]) -> Result<Report, &'static str> {
    if !newest.is_check() {
        return Err("no-check");
    }
    if ends_in_call(newest.said)
        || !final_said(newest.said).is_some_and(|r| says_done(Some(&r.text)))
    {
        return Err("no-reply");
    }
    let land = newest.is_land();
    let mut checks = vec![newest];
    let mut earlier = before.iter().rev();
    let reported = loop {
        match earlier.next() {
            Some(t) if land && t.is_land() => {}
            Some(t) if t.is_check() && !t.is_land() && t.answered_done() => checks.push(t),
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

/// What a check turn's reply says beyond the done it answered with, as a
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
    Opens {
        uuid: Option<String>,
        prompt: bool,
        meta: bool,
        at: Option<i64>,
    },
    /// A main-chain assistant row: the turn has run.
    Said,
    /// Claude Code's own main-chain `turn_duration` row, which it writes as a
    End,
}

impl Mark {
    /// Whether this row, read after a turn's own row, ENDS that turn —
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
            let meta = flag("isMeta") || flag("isCompactSummary") || flag("queueTranscriptOnly");
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
fn stamp_ms(stamp: &str) -> Option<i64> {
    let secs = aterm_types::rfc3339::parse_utc_fractional(stamp)?;
    let digits = stamp.get(20..stamp.len() - 1).unwrap_or("");
    let ms = digits
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0, |n, d| n * 10 + i64::from(d - b'0'));
    secs.checked_mul(1000)?.checked_add(ms)
}

/// Whether `line` can be a [`Mark::Opens`] or a [`Mark::End`] — a user row
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
#[derive(Clone, Debug, PartialEq, Eq)]
enum Opener {
    /// The row that started it, by its id.
    Row(String),
    /// The read holds no such row: a tail inside one long turn, which every
    Before,
    /// Its row names no id (Claude Code's always do): each read's newest.
    Newest,
}

impl Opener {
    /// The done edge's turn in its first read, `jsonl`: THE NEWEST TURN
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
    ended: bool,
    /// Where its rows stop in the read (a line's start: just past its end
    upto: Option<usize>,
}

/// Where THE TURN `opener` started ends in the transcript read `jsonl`:
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
fn upto(jsonl: String, end: End) -> String {
    match end.upto {
        Some(at) if at < jsonl.len() => jsonl[..at].to_string(),
        _ => jsonl,
    }
}

/// The done edge's transcript once it settled ([`settled`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// The tail to snapshot from, up to the done edge's turn's end (a turn begun after it left out).
    pub tail: String,
    /// The pauses waited before that ([`SETTLE_EVERY`] each).
    pub waits: u32,
    /// Whether the done edge's turn was read to its end: its end row, or a prompt typed after it.
    pub ended: bool,
}

impl Settled {
    /// What the wait came to, for the loop's journal.
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

/// [`settled`] over an injected transcript and clock.
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

/// THE DONE EDGE'S TRANSCRIPT, SETTLED: `first`, the tail of the transcript at `path` the host read at the screen's done point.
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

/// A PLAIN TURN's rule on the newest turn ([`report`]): its final message
fn by_turn(newest: &TurnRows<'_>) -> Result<Report, &'static str> {
    if ends_in_call(newest.said) {
        return Err("no-reply");
    }
    let said = final_said(newest.said).ok_or("no-reply")?;
    std::iter::once(said)
        .chain(newest.said.iter().rev().filter_map(Item::said).skip(1))
        .find_map(|r| {
            let text = without_finished_lines(&r.text);
            (!text.is_empty()).then_some(Report { text, ..r })
        })
        .ok_or("empty")
}

/// THE WHOLE EXCHANGE since the task's user row.
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

/// Where a snapshot came from, as its sidecar's one line says it ([`snapshot`], [`answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The transcript: the conversation, the row the report is, and the file it was read from.
    Transcript {
        session: String,
        uuid: String,
        transcript: Option<PathBuf>,
    },
    /// No result, and why: [`report`]'s word, or `no-transcript`.
    Unavailable {
        why: String,
        transcript: Option<PathBuf>,
        turn: Option<String>,
    },
    /// THE SCREEN of a harness whose reader alone reads its turns (agy, Muse Code, Gemini CLI: no transcript aterm reads).
    Screen { program: String },
}

impl Source {
    /// The sidecar line: `source=transcript session=<pct> uuid=<pct>` or `source=none why=<word>` (and ` turn=<pct>` where it names the turn), then ` transcript=<pct>` when one was read.
    #[must_use]
    pub fn line(&self) -> String {
        let pct = aterm_codec::pct::encode;
        let none = None;
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
            Source::Screen { program } => {
                (format!("source=screen program={}", pct(program)), &none)
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
            "screen" => Some(Source::Screen {
                program: field("program").filter(|w| is_word(w))?,
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
    /// The rule the result was read by: the one asked for, or the check's where the newest turn is a check ([`report`]).
    pub by: DoneBy,
}

/// THE PREVIOUS LANDING'S RESULT, PRUNED.
pub fn prune_results(tasks: &Path, stem: &str) -> usize {
    let Some((sid, rev)) = stem.rsplit_once('-') else {
        return 0;
    };
    let Ok(dir) = std::fs::read_dir(tasks) else {
        return 0;
    };
    let mut removed = 0;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(n) = name
            .strip_suffix(".result.md")
            .or_else(|| name.strip_suffix(".result.src"))
            .and_then(|s| s.strip_prefix(sid)?.strip_prefix('-'))
        else {
            continue;
        };
        if n != rev
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// SNAPSHOT THE RESULT, at the done edge.
///
/// # Errors
/// A `stem` that is no such stem, or the directory or a file could not be written.
pub fn snapshot(
    tasks: &Path,
    stem: &str,
    transcript: Option<(&Path, &str)>,
    by: DoneBy,
) -> std::io::Result<Snapshot> {
    check_stem(stem)?;
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
    write_snapshot(tasks, stem, &text, &source, unavailable, by)
}

/// SNAPSHOT A SCREEN'S RESULT, at the done edge of a harness aterm reads no transcript of (agy, Muse Code, Gemini CLI).
///
/// # Errors
/// As [`snapshot`].
pub fn snapshot_screen(
    tasks: &Path,
    stem: &str,
    program: &str,
    said: &str,
    by: DoneBy,
) -> std::io::Result<Snapshot> {
    check_stem(stem)?;
    let said = said.trim();
    if said.is_empty() || !is_word(program) {
        let why = "no-words";
        let source = Source::Unavailable {
            why: why.to_string(),
            transcript: None,
            turn: None,
        };
        return write_snapshot(tasks, stem, "", &source, Some(why), by);
    }
    let source = Source::Screen {
        program: program.to_string(),
    };
    write_snapshot(tasks, stem, said, &source, None, by)
}

/// The words a screen's reader read of a turn, as [`snapshot_screen`] keeps them.
#[must_use]
pub fn screen_said(turn_said: Option<&str>, said_tail: &str) -> String {
    match turn_said.map(str::trim).filter(|t| !t.is_empty()) {
        Some(before) => format!("{before}\n\n{}", said_tail.trim()),
        None => said_tail.trim().to_string(),
    }
}

/// A SCREEN-READ LANDING'S RESULT ([`snapshot_screen`]): the worker's `report`.
#[must_use]
pub fn with_report(report: Option<&str>, said: &str) -> String {
    match report
        .map(str::trim)
        .filter(|r| !r.is_empty() && *r != said.trim())
    {
        Some(report) => format!("{report}\n\n{}", said.trim()),
        None => said.to_string(),
    }
}

/// A `stem` that is one file name's stem (`[A-Za-z0-9_-]`, not empty).
fn check_stem(stem: &str) -> std::io::Result<()> {
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
    Ok(())
}

/// The result's `text` and its `source` written under `tasks` (0700, made
fn write_snapshot(
    tasks: &Path,
    stem: &str,
    text: &str,
    source: &Source,
    unavailable: Option<&'static str>,
    by: DoneBy,
) -> std::io::Result<Snapshot> {
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
    super::upgrade_models::write_atomic(&path, text)?;
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
fn tail_of(path: &Path, bytes: u64) -> Option<(String, bool)> {
    let mut f = super::footer::open_regular(path)?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf).ok()?;
    Some((String::from_utf8_lossy(&buf).into_owned(), start == 0))
}

/// THE `result [full]` REPLY over the snapshot at `path` ([`snapshot`]), framed `OK <n>` + `n` lines.
#[must_use]
pub fn answer(path: &Path, full: bool) -> String {
    match source_of(path) {
        None => unavailable("no-snapshot"),
        Some(Source::Unavailable { why, .. }) => unavailable(&why),
        Some(Source::Screen { .. }) if full => unavailable("no-transcript"),
        Some(Source::Screen { program }) => {
            let Some(text) = super::footer::read_small(path, RESULT_READ_BYTES) else {
                return unavailable("unreadable");
            };
            framed(
                &format!(
                    "source=screen program={}",
                    aterm_codec::pct::encode(&program)
                ),
                text.len(),
                "",
                &head_lines(&text, RESULT_MAX_CHARS, path),
            )
        }
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

/// A result the done edge missed, found when its turn was read again ([`reread`]).
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

    /// The snapshot at `path` and its sidecar written over with it, each through a rename and the text first, as [`snapshot`] writes them.
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

    /// The `result [full]` reply for it ([`answer`]), whether or not it was written over the snapshot at `path`.
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

/// A RESULT THE DONE EDGE MISSED, READ AGAIN.
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
    let report = read(&upto(jsonl, end), DoneBy::Turn).report.ok()?;
    Some(Reread { report, transcript })
}

#[cfg(test)]
#[path = "result_tests.rs"]
mod tests;
