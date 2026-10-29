// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker result extraction from conversation transcripts.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;

use super::upgrade::{ANNOUNCE_HEAD, is_land_act};
use crate::supervise::policy::turn_end::{is_done_check, says_done, without_finished_lines};

/// The most of a result one `result` reply prints, in characters.
pub const RESULT_MAX_CHARS: usize = 24_000;

/// The most of a transcript the result is read from, from its end.
pub const TRANSCRIPT_READ_BYTES: u64 = 16 * 1024 * 1024;

const RESULT_READ_BYTES: u64 = 16 * 1024 * 1024;

const SIDECAR_READ_BYTES: u64 = 8 * 1024;

/// The longest the done edge waits for its turn's end row ([`settled`]): a deadline, not a
/// latency budget.
pub const SETTLE_WITHIN: Duration = Duration::from_secs(5);

/// How often the done edge reads the transcript's end again while it waits.
pub const SETTLE_EVERY: Duration = Duration::from_millis(150);

const END_READ_BYTES: u64 = 256 * 1024;

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
    /// A turn's start whose prompt is absent or not yet read.
    Boundary,
    /// A Codex rollout's user message read while no turn's opening waits for its prompt (one
    /// steered into a running turn, or one before every turn's start the read holds): no turn's
    /// start, and none of its report; [`thread_of`] (`result full` over a rollout,
    /// [`rollout_reply`]) shows it as a prompt is shown.
    Steered(String),
    /// One assistant message, its rows merged by `message.id`.
    Said {
        id: Option<String>,
        texts: Vec<String>,
        uuid: String,
        session: String,
        /// The turn goes on past this message, so whatever it said is no
        /// turn's last word ([`ends_in_call`]): in a Claude Code transcript,
        /// its last content block is a tool's call (its result, then more of
        /// the model's words); in a Codex rollout, its phase is `commentary`.
        calls: Option<bool>,
    },
}

impl Item {
    const fn is_prompt(&self) -> bool {
        matches!(self, Item::Prompt(_))
    }

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

fn last_block_calls(content: Option<&Value>) -> Option<bool> {
    match content {
        Some(Value::String(_)) => Some(false),
        Some(Value::Array(parts)) => parts
            .last()
            .map(|p| p.get("type").and_then(Value::as_str) == Some("tool_use")),
        _ => None,
    }
}

fn ends_in_call(items: &[Item]) -> bool {
    items.iter().rev().find_map(|i| match i {
        Item::Said { calls, .. } => Some(*calls),
        Item::Prompt(_) | Item::Boundary | Item::Steered(_) => None,
    }) == Some(Some(true))
}

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

fn words_of(message: &Value) -> Option<String> {
    let typed = texts_of(message.get("content")).join("\n");
    let words = super::upgrade::typed_words(&typed);
    (!words.is_empty()).then(|| words.to_owned())
}

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

fn final_said(items: &[Item]) -> Option<Report> {
    items.iter().rev().find_map(Item::said)
}

struct TurnRows<'a> {
    prompt: Option<&'a str>,
    said: &'a [Item],
}

impl TurnRows<'_> {
    fn is_check(&self) -> bool {
        self.is_land() || self.prompt.is_some_and(is_done_check)
    }

    fn is_land(&self) -> bool {
        self.prompt.is_some_and(is_land_act)
    }

    fn answered_done(&self) -> bool {
        final_said(self.said).is_none_or(|r| says_done(Some(&r.text)))
    }
}

fn worker_turns(items: &[Item]) -> Vec<TurnRows<'_>> {
    let mut out = Vec::new();
    let mut prompt = None;
    let mut start = 0;
    for (at, item) in items.iter().enumerate() {
        let next_prompt = match item {
            Item::Prompt(words) => Some(words.as_str()),
            Item::Boundary => None,
            Item::Said { .. } | Item::Steered(_) => continue,
        };
        // An empty turn still separates its landing from an older report.
        if at > 0 {
            out.push(TurnRows {
                prompt,
                said: &items[start..at],
            });
        }
        prompt = next_prompt;
        start = at + 1;
    }
    out.push(TurnRows {
        prompt,
        said: &items[start..],
    });
    out.retain(|t| !t.prompt.is_some_and(|p| p.starts_with(ANNOUNCE_HEAD)));
    out
}

/// The rule a result is read by: a check's or a plain turn's.
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
    pub const fn word(self) -> &'static str {
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

/// The worker's report in the transcript tail `jsonl`, read at the done edge by rule `by`.
///
/// # Errors
/// Why there is none, as a word: `no-reply`, `no-check`, `no-report` or `empty`.
pub fn report(jsonl: &str, by: DoneBy) -> Result<Report, &'static str> {
    read(jsonl, by).report
}

struct Reading {
    /// The rule it read by: `by`, or [`DoneBy::Check`] where the newest turn
    /// is a check.
    by: DoneBy,
    /// [`report`]'s answer.
    report: Result<Report, &'static str>,
    /// Where either rule found the newest turn's reply missing or unfinished ([`LAG_MISSES`]),
    /// the user row that started the done edge's turn ([`Opener::of`]), which [`reread`] reads
    /// again once Claude Code has written the reply.
    turn: Option<String>,
}

fn read(jsonl: &str, by: DoneBy) -> Reading {
    let (by, report) = read_items(&items(jsonl), by);
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

/// [`report`] over a conversation's `items`, and the rule it read them by.
fn read_items(items: &[Item], by: DoneBy) -> (DoneBy, Result<Report, &'static str>) {
    let turns = worker_turns(items);
    let Some((newest, before)) = turns.split_last() else {
        return (by, Err("no-reply"));
    };
    if by == DoneBy::Check || newest.is_check() {
        (DoneBy::Check, by_check(newest, before))
    } else {
        (DoneBy::Turn, by_turn(newest))
    }
}

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

enum Mark {
    /// A main-chain user row with words, which can start Claude Code on a turn: a prompt
    /// [`report`] reads (`prompt`), or a row it reads past, such as a background command's
    /// `<task-notification>`, a slash command, or an `isMeta` row Claude Code starts a turn on
    /// itself (`meta`). It carries the row's id and when it was written (`at`, unix
    /// milliseconds).
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
    const fn ends_turn(&self, said: bool) -> bool {
        match self {
            Mark::End => true,
            Mark::Opens { prompt: true, .. } => true,
            Mark::Opens { meta: false, .. } => said,
            Mark::Opens { .. } | Mark::Said => false,
        }
    }
}

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

fn may_open_or_end(line: &str) -> bool {
    line.contains("\"user\"") || line.contains("turn_duration")
}

fn is_turn_end(line: &str) -> bool {
    let Ok(v) = super::transcript::usage(line) else {
        return false;
    };
    v.get("type").and_then(Value::as_str) == Some("system")
        && v.get("subtype").and_then(Value::as_str) == Some("turn_duration")
        && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
}

/// The done edge's turn, as its first read of the transcript places it ([`Opener::of`]).
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
    /// Whether it has ended: a `turn_duration` row after its row, or the next turn's start
    /// ([`Mark::ends_turn`]): a later prompt, or a tagged row after the turn's words.
    ended: bool,
    /// Where its rows stop in the read (a line's start: just past its end
    /// row, at the next turn's row, else the read's end); `None` where the
    /// read does not hold its row (it began after it), so nothing is cut.
    upto: Option<usize>,
}

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

fn upto(jsonl: String, end: End) -> String {
    match end.upto {
        Some(at) if at < jsonl.len() => jsonl[..at].to_string(),
        _ => jsonl,
    }
}

/// The done edge's transcript once it settled ([`settled`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// The tail to snapshot from, up to the done edge's turn's end.
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

/// The done edge's transcript, read again every [`SETTLE_EVERY`] until its turn's end shows,
/// [`SETTLE_WITHIN`] passes or `stopping` says to stop.
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

/// The whole exchange since the task's user row, and whether the read holds that row.
#[must_use]
pub fn thread(jsonl: &str) -> (String, bool) {
    thread_of(&items(jsonl))
}

/// [`thread`] over a conversation's `items`: each prompt (and each message steered into a turn)
/// quoted `> `, each message's words as they are.
fn thread_of(items: &[Item]) -> (String, bool) {
    let start = items.iter().position(Item::is_prompt);
    let mut out = String::new();
    for item in &items[start.unwrap_or(0)..] {
        let block = match item {
            Item::Prompt(t) | Item::Steered(t) => t
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
    /// The screen of a harness whose reader alone reads its turns.
    Screen { program: String },
    /// A Codex rollout ([`snapshot_rollout`]): the conversation, the turn the
    /// report is the last words of, and the rollout it was read from.
    Rollout {
        thread: String,
        turn: String,
        rollout: Option<PathBuf>,
    },
    /// No result of a Codex rollout ([`snapshot_rollout`]), and why ([`report`]'s word, or
    /// `no-transcript` where no rollout was read), with what [`reread`] reads it again by: the
    /// conversation, the turn the landing read where the rollout named it, the rollout where one
    /// was read, the `$CODEX_HOME` the conversation's rollout is found under where the landing
    /// knew it, and when the landing was seen (unix ms).
    RolloutUnavailable {
        why: String,
        thread: String,
        turn: Option<String>,
        rollout: Option<PathBuf>,
        home: Option<PathBuf>,
        landed: Option<i64>,
    },
}

impl Source {
    /// The sidecar line: `source=transcript session=<pct> uuid=<pct>`, `source=none why=<word>`
    /// (with ` turn=<pct>` where it names the turn), `source=screen program=<word>`,
    /// `source=rollout thread=<pct> turn=<pct>` (with ` rollout=<pct>`) or, for a rollout's
    /// missing result, `source=none why=<word> thread=<pct>` (with ` turn=<pct>`, ` rollout=<pct>`,
    /// ` home=<pct>` and ` landed=<ms>` where it has them), then ` transcript=<pct>` when a
    /// transcript was read. A reader that predates the rollout's `source=none` fields reads that
    /// line as a `source=none` with no transcript: the same unavailable result, never read again.
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
            Source::Rollout {
                thread,
                turn,
                rollout,
            } => {
                let mut line = format!("source=rollout thread={} turn={}", pct(thread), pct(turn));
                if let Some(path) = rollout {
                    line.push_str(" rollout=");
                    line.push_str(&pct(&path.to_string_lossy()));
                }
                (line, &none)
            }
            Source::RolloutUnavailable {
                why,
                thread,
                turn,
                rollout,
                home,
                landed,
            } => {
                let mut line = format!("source=none why={} thread={}", pct(why), pct(thread));
                if let Some(turn) = turn {
                    line.push_str(" turn=");
                    line.push_str(&pct(turn));
                }
                for (key, path) in [(" rollout=", rollout), (" home=", home)] {
                    if let Some(path) = path {
                        line.push_str(key);
                        line.push_str(&pct(&path.to_string_lossy()));
                    }
                }
                if let Some(ms) = landed {
                    line.push_str(" landed=");
                    line.push_str(&ms.to_string());
                }
                (line, &none)
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
            "none" => {
                let why = field("why").filter(|w| is_word(w))?;
                let turn = field("turn");
                Some(match field("thread") {
                    None => Source::Unavailable {
                        why,
                        transcript,
                        turn,
                    },
                    Some(thread) => Source::RolloutUnavailable {
                        why,
                        thread,
                        turn,
                        rollout: field("rollout").map(PathBuf::from),
                        home: field("home").map(PathBuf::from),
                        landed: field("landed").and_then(|ms| ms.parse().ok()),
                    },
                })
            }
            "screen" => Some(Source::Screen {
                program: field("program").filter(|w| is_word(w))?,
            }),
            "rollout" => Some(Source::Rollout {
                thread: field("thread")?,
                turn: field("turn")?,
                rollout: field("rollout").map(PathBuf::from),
            }),
            _ => None,
        }
    }
}

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
    /// The rule the result was read by: the one asked for, or the check's where the newest turn
    /// is a check.
    pub by: DoneBy,
}

/// Removes this session's result revisions older than `stem`'s and returns how many; equal,
/// newer and unparseable revisions stay, so a delayed capture never removes a newer result.
pub fn prune_results(tasks: &Path, stem: &str) -> usize {
    let Some((sid, rev)) = stem.rsplit_once('-') else {
        return 0;
    };
    if rev.is_empty() || !rev.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    let Ok(rev) = rev.parse::<u64>() else {
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
        if !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.parse::<u64>().is_ok_and(|n| n < rev)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// The result file's stem for the task the record at `rev` stands for, in
/// session `sel` (`@s-…`): `<gsid>-<rev>`, the task file's own pairing.
#[must_use]
pub fn result_stem(sel: Option<&str>, rev: u64) -> String {
    let sid: String = sel
        .unwrap_or_default()
        .trim_start_matches('@')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let sid = if sid.is_empty() { "session" } else { &sid };
    format!("{sid}-{rev}")
}

/// Snapshots the result at the done edge.
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

/// Snapshots a screen's result at the done edge of a harness aterm reads no transcript of.
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

/// A screen-read landing's result ([`snapshot_screen`]): the worker's `report` ahead of `said`,
/// unless it is empty or the same words.
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

fn unavailable(why: &str) -> String {
    format!("ERR result unavailable why={why}\n")
}

fn framed(stamp: &str, bytes: usize, extra: &str, lines: &[String]) -> String {
    let mut out = format!("OK {} {stamp} bytes={bytes}{extra}\n", lines.len());
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

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

fn tail_lines(text: &str, max: usize, file: &Path, noun: &str, whole: bool) -> Vec<String> {
    let skip = text.chars().count().saturating_sub(max);
    let at = text
        .char_indices()
        .nth(skip)
        .map_or(text.len(), |(at, _)| at);
    let shown = &text[at..];
    let mut lines = Vec::new();
    if skip > 0 || !whole {
        lines.push(format!(
            "… earlier turns cut; the {noun}: {}",
            file.display()
        ));
    }
    lines.extend(
        shown
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()),
    );
    lines
}

fn tail_of(path: &Path, bytes: u64) -> Option<(String, bool)> {
    let mut f = super::footer::open_regular(path)?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf).ok()?;
    Some((String::from_utf8_lossy(&buf).into_owned(), start == 0))
}

/// The `result [full]` reply over the snapshot at `path` ([`snapshot`]), framed `OK <n>` and
/// `n` lines.
#[must_use]
pub fn answer(path: &Path, full: bool) -> String {
    match source_of(path) {
        None => unavailable("no-snapshot"),
        Some(Source::Unavailable { why, .. } | Source::RolloutUnavailable { why, .. }) => {
            unavailable(&why)
        }
        Some(Source::Screen { .. }) if full => unavailable("no-transcript"),
        Some(Source::Rollout {
            thread,
            turn,
            rollout,
        }) => rollout_reply(&thread, &turn, rollout.as_deref(), path, full, || {
            super::footer::read_small(path, RESULT_READ_BYTES)
        }),
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

fn source_of(path: &Path) -> Option<Source> {
    super::footer::read_small(&sidecar_of(path), SIDECAR_READ_BYTES)
        .and_then(|t| Source::parse(t.lines().next()?))
}

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
        &tail_lines(
            &thread,
            RESULT_MAX_CHARS,
            file,
            "transcript",
            whole && began,
        ),
    )
}

/// [`reply`] for a Codex rollout's result: `result` frames the snapshot's `text` under
/// `source=rollout`, and `result full` the whole exchange the `rollout` holds since the task's
/// prompt, read at request time and bounded as a transcript's is (`no-prompt` where the rollout
/// was read whole and holds none); `no-transcript` where the snapshot names no rollout or it
/// does not read.
fn rollout_reply(
    thread: &str,
    turn: &str,
    rollout: Option<&Path>,
    path: &Path,
    full: bool,
    text: impl FnOnce() -> Option<String>,
) -> String {
    let pct = aterm_codec::pct::encode;
    let stamp = format!("source=rollout thread={} turn={}", pct(thread), pct(turn));
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
    let Some(file) = rollout else {
        return unavailable("no-transcript");
    };
    let Some((jsonl, whole)) = tail_of(file, TRANSCRIPT_READ_BYTES) else {
        return unavailable("no-transcript");
    };
    let (exchange, began) = thread_of(&codex_items(&jsonl, thread));
    if whole && !began {
        return unavailable("no-prompt");
    }
    framed(
        &stamp,
        exchange.len(),
        " full=1",
        &tail_lines(&exchange, RESULT_MAX_CHARS, file, "rollout", whole && began),
    )
}

/// What a result is read from, and so the reader [`reread`] and `result full` read it with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conversation {
    /// A Claude Code transcript ([`items`]).
    Transcript,
    /// A Codex rollout ([`codex_items`]): the report's `session` is its thread, its `uuid` the
    /// turn.
    Rollout,
}

/// A result the done edge missed, found when its turn was read again ([`reread`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reread {
    /// The report, as its turn's rule reads it now.
    pub report: Report,
    /// The transcript (or rollout, [`Self::from`]) it was read from.
    pub transcript: PathBuf,
    /// What that file is.
    pub from: Conversation,
}

impl Reread {
    /// The result's size: what the record's `result_bytes=` says.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.report.text.len() as u64
    }

    fn source(&self) -> Source {
        match self.from {
            Conversation::Transcript => Source::Transcript {
                session: self.report.session.clone(),
                uuid: self.report.uuid.clone(),
                transcript: Some(self.transcript.clone()),
            },
            Conversation::Rollout => Source::Rollout {
                thread: self.report.session.clone(),
                turn: self.report.uuid.clone(),
                rollout: Some(self.transcript.clone()),
            },
        }
    }

    /// Writes it over the snapshot at `path` and its sidecar as [`snapshot`] does: text first,
    /// each through a rename.
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

    /// The `result [full]` reply for it ([`answer`]), whether or not it was written over the
    /// snapshot at `path`.
    #[must_use]
    pub fn answer(&self, path: &Path, full: bool) -> String {
        let (session, uuid) = (&self.report.session, &self.report.uuid);
        let text = || Some(self.report.text.clone());
        match self.from {
            Conversation::Transcript => {
                reply(session, uuid, Some(&self.transcript), path, full, text)
            }
            Conversation::Rollout => {
                rollout_reply(session, uuid, Some(&self.transcript), path, full, text)
            }
        }
    }
}

/// Reads again, for its turn alone, a result the done edge missed because the turn's reply was
/// not yet written (`no-reply` or `no-marker`); `None` where there is no such miss, or the
/// transcript no longer holds the turn's row or still has no report. A Codex rollout's miss is
/// read again by [`reread_rollout`], with no claim to name a conversation its landing knew none
/// of ([`reread_with`]).
#[must_use]
pub fn reread(path: &Path) -> Option<Reread> {
    reread_with(path, || None)
}

/// [`reread`] with the worker's `claim` as it stands now: the `$CODEX_HOME` and the thread a
/// Codex landing's resolver names (its first step, the claim's conversation), asked only where
/// the landing read no rollout and knew no conversation to find one by.
#[must_use]
pub fn reread_with(
    path: &Path,
    claim: impl FnOnce() -> Option<(PathBuf, String)>,
) -> Option<Reread> {
    let source = source_of(path);
    if let Some(Source::RolloutUnavailable {
        why,
        thread,
        turn,
        rollout,
        home,
        landed,
    }) = source
    {
        return reread_rollout(
            &why,
            &thread,
            turn.as_deref(),
            rollout,
            home.as_deref(),
            landed,
            claim,
        );
    }
    let Some(Source::Unavailable {
        why,
        transcript: Some(transcript),
        turn: Some(turn),
    }) = source
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
    Some(Reread {
        report,
        transcript,
        from: Conversation::Transcript,
    })
}

/// A CODEX LANDING'S MISSED RESULT, READ AGAIN FOR ITS TURN ALONE: a miss because the turn's
/// reply was not yet written (`no-reply`, `no-marker`) is read from the `rollout` the landing
/// read, and one with no rollout read (`no-transcript`) from the rollout the landing's resolver
/// finds now ([`super::codex_tap::rollout_path`]): of `thread` under `home` where the landing knew
/// them, else of the conversation the worker's `claim` names now (a thread id, the landing's own
/// where it named one). The rollout is read up to the end of the landing's turn
/// ([`codex_turn_upto`]) by its turn's rule, so a later turn's reply is never taken. `None` for
/// any other miss, no conversation to find, a rollout that does not read, a landing's turn the
/// read no longer holds (a conversation begun after the landing holds none), or a turn with still
/// no report.
fn reread_rollout(
    why: &str,
    thread: &str,
    turn: Option<&str>,
    rollout: Option<PathBuf>,
    home: Option<&Path>,
    landed: Option<i64>,
    claim: impl FnOnce() -> Option<(PathBuf, String)>,
) -> Option<Reread> {
    let is_thread_id = super::codex_rollout::is_thread_id;
    let (file, thread) = match why {
        "no-transcript" => {
            let (home, thread) = match home.filter(|_| is_thread_id(thread)) {
                Some(home) => (home.to_path_buf(), thread.to_string()),
                None => claim().filter(|(_, named)| {
                    is_thread_id(named) && (!is_thread_id(thread) || named == thread)
                })?,
            };
            (super::codex_tap::rollout_path(&home, &thread)?, thread)
        }
        why if LAG_MISSES.contains(&why) => (rollout?, thread.to_string()),
        _ => return None,
    };
    let jsonl = super::codex_rollout::tail_of(&file, TRANSCRIPT_READ_BYTES)?;
    let end = codex_turn_upto(&jsonl, turn, landed)?;
    let report = read_items(&codex_items(&jsonl[..end], &thread), DoneBy::Turn)
        .1
        .ok()?;
    Some(Reread {
        report,
        transcript: file,
        from: Conversation::Rollout,
    })
}

/// Where the landing's turn ends in the Codex rollout tail `jsonl` (a line's start: the next
/// `task_started` after its own, else the read's end): the turn whose `task_started` names
/// `turn`, or, where the landing named none, the newest one begun by `landed` (unix ms, its row's
/// `timestamp`). `None` where the read holds no such start.
fn codex_turn_upto(jsonl: &str, turn: Option<&str>, landed: Option<i64>) -> Option<usize> {
    let mut at = 0;
    let mut from = None;
    let mut upto = None;
    for line in jsonl.split_inclusive('\n') {
        let next = at + line.len();
        if let Some((id, stamp)) = codex_start(line) {
            if from.is_some() && upto.is_none() {
                upto = Some(at);
            }
            let ours = match turn {
                Some(turn) => id.as_deref() == Some(turn),
                None => landed
                    .zip(stamp)
                    .is_some_and(|(landed, began)| began <= landed),
            };
            if ours {
                (from, upto) = (Some(next), None);
            }
        }
        at = next;
    }
    from?;
    Some(upto.unwrap_or(jsonl.len()))
}

/// A Codex rollout row that starts a turn (`task_started`): the turn it names, and when it was
/// written (its row's `timestamp`, unix ms).
fn codex_start(line: &str) -> Option<(Option<String>, Option<i64>)> {
    if !line.contains("\"task_started\"") {
        return None;
    }
    let v = aterm_json::from_str::<Value>(line.trim_end()).ok()?;
    let payload = v.get("payload")?;
    (v.get("type").and_then(Value::as_str) == Some("event_msg")
        && payload.get("type").and_then(Value::as_str) == Some("task_started"))
    .then(|| {
        (
            payload
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            v.get("timestamp")
                .and_then(Value::as_str)
                .and_then(stamp_ms),
        )
    })
}

/// The newest turn the Codex rollout tail `jsonl` names a start of ([`codex_start`]).
fn codex_newest_turn(jsonl: &str) -> Option<String> {
    jsonl.lines().rev().find_map(codex_start)?.0
}

/// A Codex rollout's conversation `thread` as [`items`] reads a transcript's: each turn opens at
/// its `task_started`, its prompt is the first user message that is no context Codex added
/// ([`codex_context`]), a later user message's words are steered into it ([`Item::Steered`]), and
/// each assistant message is its words (a `commentary` one marks a turn that goes on past it).
fn codex_items(jsonl: &str, thread: &str) -> Vec<Item> {
    let mut out = Vec::new();
    let mut turn = String::from("-");
    let mut opening = None;
    for line in jsonl.lines() {
        if !line.contains("\"message\"") && !line.contains("\"task_started\"") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = v.get("payload") else {
            continue;
        };
        let texts = |part: &str| -> Vec<String> {
            payload
                .get("content")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|p| p.get("type").and_then(Value::as_str) == Some(part))
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        match (
            v.get("type").and_then(Value::as_str),
            payload.get("type").and_then(Value::as_str),
            payload.get("role").and_then(Value::as_str),
        ) {
            (Some("event_msg"), Some("task_started"), _) => {
                payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .clone_into(&mut turn);
                opening = Some(out.len());
                out.push(Item::Boundary);
            }
            (Some("response_item"), Some("message"), Some("user")) => {
                let mut parts = texts("input_text");
                parts.retain(|p| !codex_media_label(p));
                if parts.iter().any(|p| codex_context(p)) {
                    continue;
                }
                let words = parts.join("\n").trim().to_string();
                if words.is_empty() {
                    continue;
                }
                match opening.take() {
                    Some(at) => out[at] = Item::Prompt(words),
                    None => out.push(Item::Steered(words)),
                }
            }
            (Some("response_item"), Some("message"), Some("assistant")) => {
                opening = None;
                out.push(Item::Said {
                    id: None,
                    texts: texts("output_text"),
                    uuid: turn.clone(),
                    session: thread.to_string(),
                    calls: Some(payload.get("phase").and_then(Value::as_str) == Some("commentary")),
                });
            }
            _ => {}
        }
    }
    out
}

/// Whether a user message's text is context Codex adds to a turn itself: its
/// tagged fragments (`<environment_context>`, `<user_shell_command>`, …), but
/// no media label ([`codex_media_label`]), and the project's `AGENTS.md`
/// (`# AGENTS.md instructions`, ` for <dir>` where it names one).
pub(crate) fn codex_context(text: &str) -> bool {
    let text = text.trim_start();
    (text.starts_with('<') && !codex_media_label(text))
        || text.starts_with("# AGENTS.md instructions")
}

/// Whether a user message's text part is a label Codex puts around a file the
/// person attached (`<image name=…>`, `</image>`, `<audio>`, …): part of their
/// own message, and none of its words.
pub(crate) fn codex_media_label(text: &str) -> bool {
    let text = text.trim();
    [
        "<image>",
        "<image name=",
        "</image>",
        "<audio>",
        "<audio name=",
        "</audio>",
    ]
    .iter()
    .any(|label| text.starts_with(label))
}

/// The worker's report in the Codex rollout tail `jsonl` of conversation `thread`, read at the
/// done edge by rule `by` as [`report`] reads a transcript's.
///
/// # Errors
/// As [`report`].
pub fn codex_report(jsonl: &str, thread: &str, by: DoneBy) -> Result<Report, &'static str> {
    read_items(&codex_items(jsonl, thread), by).1
}

/// Whether the newest turn in the Codex rollout tail `jsonl` has ended: a `task_complete` or
/// `turn_aborted` after its `task_started` that names the same turn, or where either names no
/// turn. A previous turn's late terminal or message text never ends it; a tail with no start
/// reads as ended.
#[must_use]
pub fn codex_turn_ended(jsonl: &str) -> bool {
    let mut terminals: Vec<Option<String>> = Vec::new();
    for line in jsonl.lines().rev() {
        if !line.contains("\"event_msg\"") {
            continue;
        }
        let Ok(event) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let Some(payload) = event.get("payload") else {
            continue;
        };
        let turn = payload.get("turn_id").and_then(Value::as_str);
        match payload.get("type").and_then(Value::as_str) {
            Some("task_started") => {
                return terminals.iter().any(|terminal| {
                    turn.is_none_or(|turn| terminal.as_deref().is_none_or(|ended| ended == turn))
                });
            }
            Some("task_complete" | "turn_aborted") => {
                terminals.push(turn.map(str::to_owned));
            }
            _ => {}
        }
    }
    true
}

/// Snapshots a Codex worker's result at its landing's done edge, seen at `landed` (unix ms): its
/// report in the tail of its `rollout` (the file and the text read from it) of conversation
/// `thread`, whose rollout is found under `home` (its `$CODEX_HOME`, where the landing knew it).
/// A missing result keeps what [`reread`] reads it again by ([`Source::RolloutUnavailable`]): the
/// rollout and the turn its tail names last, or with no rollout read, the thread and its home
/// (`-` and none where the landing knew no conversation, found then by the worker's claim at
/// [`reread_with`]).
///
/// # Errors
/// As [`snapshot`].
pub fn snapshot_rollout(
    tasks: &Path,
    stem: &str,
    rollout: Option<(&Path, &str)>,
    thread: &str,
    home: Option<&Path>,
    landed: i64,
    by: DoneBy,
) -> std::io::Result<Snapshot> {
    check_stem(stem)?;
    let none = |why: &'static str, read: Option<(&Path, &str)>, by: DoneBy| {
        (
            String::new(),
            Source::RolloutUnavailable {
                why: why.to_string(),
                thread: thread.to_string(),
                turn: read.and_then(|(_, jsonl)| codex_newest_turn(jsonl)),
                rollout: read.map(|(file, _)| file.to_path_buf()),
                home: home.map(Path::to_path_buf),
                landed: Some(landed),
            },
            Some(why),
            by,
        )
    };
    let (text, source, unavailable, by) = match rollout {
        None => none("no-transcript", None, by),
        Some((file, jsonl)) => match read_items(&codex_items(jsonl, thread), by) {
            (by, Ok(r)) => (
                r.text,
                Source::Rollout {
                    thread: r.session,
                    turn: r.uuid,
                    rollout: Some(file.to_path_buf()),
                },
                None,
                by,
            ),
            (by, Err(why)) => none(why, rollout, by),
        },
    };
    write_snapshot(tasks, stem, &text, &source, unavailable, by)
}

#[cfg(test)]
#[path = "result_tests.rs"]
mod tests;
