// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor decision journal ([`super::SuperviseOpts::journal`]): one JSON record per line a
//! loop prints ([`JournalRecord::of_line`]). A record's `kind` is one of [`Word::ALL`]'s names,
//! or `other` for a line that begins with any other word (each a loop journals is one of
//! [`Noted::ALL`]):
//!
//! ```text
//! {"t":<unix ms>,"sid":"<sid>"|null,
//!  "kind":"event|approved|declined|dismissed|reconnect|timeout|exit|mail|extend|escalated|
//!          cleared|continued|typed|restarted|waiting|skipped|unverified|limited|unproven|
//!          dialog|chose|exception|probed|reset|probe|needs|hold|occupancy|other",
//!  "phase":"idle|question|prompt|limited|survey|context|compacted|turn|idle-no-report|
//!           <rule id>|resumed|still-limited|rebriefed|rebrief-failed|point|background|-",
//!  "seq":<n>|null,"complete":0|1|null,"rows":<n>|null,"summary":"<the line's free-text tail>",
//!  "line":"<the exact line>","turn":<id>|null,"report":<id>|null[,"mark":"<16 hex>"]
//!  [,"ended":<unix ms>]}
//! ```
//!
//! A line's free-text tail is the agent's screen by construction (a reply's last line, a box's
//! command, a question's words), so it stays in the harness's drive store, 0600 files in the 0700
//! `<state>/drive` folder: this journal, the approval ledger beside it (which keeps an act's
//! command and the answer typed) and tapes when recorded (DESIGN-session-log §3.6). An app log
//! keeps [`log_words`] of a line: its words, sequence and the tail's length.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::screen::Json;

/// The line that adopts a badge a previous loop left on the session, at this loop's start.
pub const ADOPTED: &str = "ADOPTED";

/// The badge a `CLEARED seq=<n> box attention=<reply>` line ended.
pub const CLEARED_BOX: &str = "box";
/// … `CLEARED seq=<n> turn-end attention=<reply>`.
pub const CLEARED_TURN_END: &str = "turn-end";
/// … `CLEARED seq=<n> goal attention=<reply>`.
pub const CLEARED_GOAL: &str = "goal";
/// … `CLEARED seq=<n> memory attention=<reply>: …`.
pub const CLEARED_MEMORY: &str = "memory";
/// … `CLEARED seq=<n> stale attention=<reply> (…)`.
pub const CLEARED_STALE: &str = "stale";
/// … `CLEARED seq=- hidden attention=<reply> (…)`.
pub const CLEARED_HIDDEN: &str = "hidden";
/// The line that puts the worker's NEEDS YOU on the person's screen.
pub const NEEDS: &str = "NEEDS";
/// … `CLEARED seq=<n> needs attention=<reply> <why>`.
pub const CLEARED_NEEDS: &str = "needs";

/// Every word a `CLEARED` line names a badge by.
pub const CLEARED_BADGES: [&str; 6] = [
    CLEARED_BOX,
    CLEARED_TURN_END,
    CLEARED_GOAL,
    CLEARED_MEMORY,
    CLEARED_STALE,
    CLEARED_HIDDEN,
];

/// The `<reply>` a badge's `CLEARED` line names where the loop's entry was not taken.
pub const ATTENTION_KEPT: &str = "kept";

/// The line a stall's hold writes (`stall.rs`'s `hold_for_stall`).
pub const FROZEN: &str = "FROZEN";

/// How the line of a turn the Mac slept through in the vendor's API retry, Escaped once the API
/// was back, begins (`supervise::run::woken_turn`).
pub const UNSTUCK: &str = "UNSTUCK";

/// How an `ESCALATED seq=<n> attention=<reply>` line's tail begins where no badge went.
pub const ATTENTION_SKIPPED: &str = "attention=skipped: ";
/// Why [`ATTENTION_SKIPPED`] behind another supervisor's claim.
pub const ESCALATED_BEHIND: &str = "another supervisor";
/// The `again=<why>` an `ESCALATED` line ends with where its badge is the same ask set again.
pub const AGAIN_RETRY: &str = "the retry hit the wall again";
/// The `again=<why>` of a limit episode's escalation over a badge adopted from a previous loop.
pub const AGAIN_ADOPTED: &str = "adopted from a previous watcher";
/// The `again=<why>` of the turn-end policy's badge set again as an Autopilot Off park ends.
pub const AGAIN_UNPARKED: &str = "set again after an off park";

/// Every word a journaled line is read by: the [`JournalRecord::kind`] of a line that begins
/// with it. A line that begins with any other word is `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Word {
    Event,
    Dismissed,
    Mail,
    Approved,
    Reconnect,
    Timeout,
    Exit,
    Extend,
    Escalated,
    Cleared,
    Continued,
    Typed,
    Restarted,
    Waiting,
    Skipped,
    Unverified,
    Limited,
    Unproven,
    Dialog,
    Declined,
    Chose,
    Exception,
    Probed,
    Reset,
    Needs,
    Hold,
    Occupancy,
    Probe,
}

impl Word {
    /// Every word, in declaration order.
    pub const ALL: [Self; 28] = [
        Self::Event,
        Self::Dismissed,
        Self::Mail,
        Self::Approved,
        Self::Reconnect,
        Self::Timeout,
        Self::Exit,
        Self::Extend,
        Self::Escalated,
        Self::Cleared,
        Self::Continued,
        Self::Typed,
        Self::Restarted,
        Self::Waiting,
        Self::Skipped,
        Self::Unverified,
        Self::Limited,
        Self::Unproven,
        Self::Dialog,
        Self::Declined,
        Self::Chose,
        Self::Exception,
        Self::Probed,
        Self::Reset,
        Self::Needs,
        Self::Hold,
        Self::Occupancy,
        Self::Probe,
    ];

    /// The record's `kind`: the printed word lowercased (`RECONNECTED` is `reconnect`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Dismissed => "dismissed",
            Self::Mail => "mail",
            Self::Approved => "approved",
            Self::Reconnect => "reconnect",
            Self::Timeout => "timeout",
            Self::Exit => "exit",
            Self::Extend => "extend",
            Self::Escalated => "escalated",
            Self::Cleared => "cleared",
            Self::Continued => "continued",
            Self::Typed => "typed",
            Self::Restarted => "restarted",
            Self::Waiting => "waiting",
            Self::Skipped => "skipped",
            Self::Unverified => "unverified",
            Self::Limited => "limited",
            Self::Unproven => "unproven",
            Self::Dialog => "dialog",
            Self::Declined => "declined",
            Self::Chose => "chose",
            Self::Exception => "exception",
            Self::Probed => "probed",
            Self::Reset => "reset",
            Self::Needs => "needs",
            Self::Hold => "hold",
            Self::Occupancy => "occupancy",
            Self::Probe => "probe",
        }
    }

    /// The word a line begins with as the loops print it ([`Self::printed`]; `RECONNECTED`
    /// is [`Self::Reconnect`] too).
    #[must_use]
    pub fn of_printed(word: &str) -> Option<Self> {
        match word {
            "RECONNECTED" => Some(Self::Reconnect),
            NEEDS => Some(Self::Needs),
            _ => Self::ALL.into_iter().find(|w| w.is_printed(word)),
        }
    }

    /// Whether `word` is [`Self::printed`], without building it.
    fn is_printed(self, word: &str) -> bool {
        let name = self.name();
        name.len() == word.len()
            && name
                .bytes()
                .zip(word.bytes())
                .all(|(n, w)| n.to_ascii_uppercase() == w)
    }

    /// How a line that is read as this word begins: its name in capitals.
    #[must_use]
    pub fn printed(self) -> String {
        self.name().to_ascii_uppercase()
    }

    /// The word whose [`Self::name`] a read record's `kind` is (`None`: `other`, or a word
    /// this build does not know).
    #[must_use]
    pub fn of_kind(kind: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name() == kind)
    }
}

/// Every other word a loop journals a line with. Its record's `kind` stays `other` (readers
/// read these lines' first word) and a reader names it by [`Self::of_line`]; with [`Word`] it
/// covers every word a journal holds (a test build refuses any other, [`Journal::record`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Noted {
    Done,
    Left,
    Parked,
    Unparked,
    Halted,
    Unhalted,
    Refused,
    Held,
    HoldEnded,
    Adopted,
    Kept,
    Watching,
    Claim,
    Claimed,
    NotEscalated,
    Warn,
    Task,
    Fact,
    Launch,
    Host,
    Deferred,
    Memory,
    Stalled,
    Stuck,
    Frozen,
    Thawed,
    StallWarned,
    Signalled,
    RestartRefused,
    Note,
    Sleep,
    Abandoned,
    RepoUnread,
    Unstuck,
}

impl Noted {
    /// Every word, in declaration order.
    pub const ALL: [Self; 34] = [
        Self::Done,
        Self::Left,
        Self::Parked,
        Self::Unparked,
        Self::Halted,
        Self::Unhalted,
        Self::Refused,
        Self::Held,
        Self::HoldEnded,
        Self::Adopted,
        Self::Kept,
        Self::Watching,
        Self::Claim,
        Self::Claimed,
        Self::NotEscalated,
        Self::Warn,
        Self::Task,
        Self::Fact,
        Self::Launch,
        Self::Host,
        Self::Deferred,
        Self::Memory,
        Self::Stalled,
        Self::Stuck,
        Self::Frozen,
        Self::Thawed,
        Self::StallWarned,
        Self::Signalled,
        Self::RestartRefused,
        Self::Note,
        Self::Sleep,
        Self::Abandoned,
        Self::RepoUnread,
        Self::Unstuck,
    ];

    /// How a line of this word begins (`NOT ESCALATED` is two words).
    #[must_use]
    pub const fn printed(self) -> &'static str {
        match self {
            Self::Done => "DONE",
            Self::Left => "LEFT",
            Self::Parked => "PARKED",
            Self::Unparked => "UNPARKED",
            Self::Halted => "HALTED",
            Self::Unhalted => "UNHALTED",
            Self::Refused => "REFUSED",
            Self::Held => "HELD",
            Self::HoldEnded => "HOLD-ENDED",
            Self::Adopted => ADOPTED,
            Self::Kept => "KEPT",
            Self::Watching => "WATCHING",
            Self::Claim => "CLAIM",
            Self::Claimed => "CLAIMED",
            Self::NotEscalated => "NOT ESCALATED",
            Self::Warn => "WARN",
            Self::Task => "TASK",
            Self::Fact => "FACT",
            Self::Launch => "LAUNCH",
            Self::Host => "HOST",
            Self::Deferred => "DEFERRED",
            Self::Memory => "MEMORY",
            Self::Stalled => "STALLED",
            Self::Stuck => "STUCK",
            Self::Frozen => FROZEN,
            Self::Thawed => "THAWED",
            Self::StallWarned => "STALL-WARNED",
            Self::Signalled => "SIGNALLED",
            Self::RestartRefused => "RESTART-REFUSED",
            Self::Note => "NOTE",
            Self::Sleep => "SLEEP",
            Self::Abandoned => "ABANDONED",
            Self::RepoUnread => "REPO UNREAD",
            Self::Unstuck => UNSTUCK,
        }
    }

    /// The word in lower case, a space as `-` (`not-escalated`): a reader's name for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Left => "left",
            Self::Parked => "parked",
            Self::Unparked => "unparked",
            Self::Halted => "halted",
            Self::Unhalted => "unhalted",
            Self::Refused => "refused",
            Self::Held => "held",
            Self::HoldEnded => "hold-ended",
            Self::Adopted => "adopted",
            Self::Kept => "kept",
            Self::Watching => "watching",
            Self::Claim => "claim",
            Self::Claimed => "claimed",
            Self::NotEscalated => "not-escalated",
            Self::Warn => "warn",
            Self::Task => "task",
            Self::Fact => "fact",
            Self::Launch => "launch",
            Self::Host => "host",
            Self::Deferred => "deferred",
            Self::Memory => "memory",
            Self::Stalled => "stalled",
            Self::Stuck => "stuck",
            Self::Frozen => "frozen",
            Self::Thawed => "thawed",
            Self::StallWarned => "stall-warned",
            Self::Signalled => "signalled",
            Self::RestartRefused => "restart-refused",
            Self::Note => "note",
            Self::Sleep => "sleep",
            Self::Abandoned => "abandoned",
            Self::RepoUnread => "repo-unread",
            Self::Unstuck => "unstuck",
        }
    }

    /// The word `line` begins with: its [`Self::printed`] form, then a space or the end.
    #[must_use]
    pub fn of_line(line: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| {
            line.strip_prefix(w.printed())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
    }
}

/// One journaled line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    /// When it was printed, Unix milliseconds.
    pub t: i64,
    /// The session the loop watched (the `@sid` it was given, without the `@`).
    pub sid: Option<String>,
    /// The line's leading word lowercased (`RECONNECTED` is `reconnect`).
    pub kind: String,
    /// The phase the line is about (an `EVENT`'s second word, `prompt` for an `APPROVED`).
    pub phase: String,
    /// The line's `seq=`.
    pub seq: Option<u64>,
    /// The line's `complete=` (an older build's `watch --report`).
    pub complete: Option<bool>,
    /// The line's `rows=` (an older build's `watch --report`).
    pub rows: Option<u64>,
    /// The line's free-text tail.
    pub summary: String,
    /// The line exactly as printed.
    pub line: String,
    /// The ledger turn the point's report counted from.
    pub turn: Option<u64>,
    /// The inbox row id of the report folded into an `EVENT turn` line.
    pub report: Option<u64>,
    /// The mark of the worker's last words at a turn end (`policy::turn_end::said_mark`).
    pub mark: Option<u64>,
    /// The time the end this note restates was first noted (unix ms).
    pub ended: Option<i64>,
}

/// The first word of `s` and the rest after one space.
fn split_word(s: &str) -> (&str, &str) {
    s.split_once(' ').unwrap_or((s, ""))
}

/// `key=<digits>` at the front of `s`: the number and the rest.
fn take_num<'s>(s: &'s str, key: &str) -> Option<(u64, &'s str)> {
    let (word, rest) = split_word(s);
    let v = word.strip_prefix(key)?.strip_prefix('=')?;
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((v.parse().ok()?, rest))
}

impl JournalRecord {
    /// The record of `line`, printed at `t`, read from the words the loop prints (module doc).
    pub fn of_line(t: i64, sid: Option<&str>, line: &str, turn: Option<u64>) -> Self {
        let mut rec = JournalRecord {
            t,
            sid: sid.map(|s| s.trim_start_matches('@').to_string()),
            kind: "other".to_string(),
            phase: "-".to_string(),
            seq: None,
            complete: None,
            rows: None,
            summary: String::new(),
            line: line.to_string(),
            turn,
            report: None,
            mark: None,
            ended: None,
        };
        let (word, rest) = split_word(line);
        let Some(word) = Word::of_printed(word) else {
            rec.summary = line.to_string();
            return rec;
        };
        rec.kind = word.name().to_string();
        let mut tail = rest;
        match word {
            Word::Event | Word::Dismissed => {
                let (phase, after) = split_word(rest);
                rec.phase = phase.to_string();
                tail = after;
                if let Some((seq, after)) = take_num(tail, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                if let Some((c, after)) = take_num(tail, "complete")
                    && let Some((n, after)) = take_num(after, "rows")
                {
                    rec.complete = Some(c == 1);
                    rec.rows = Some(n);
                    tail = after;
                } else if let Some((id, after)) = take_num(tail, "report")
                    && let Some((n, after)) = take_num(after, "rows")
                {
                    rec.report = Some(id);
                    rec.rows = Some(n);
                    tail = after;
                }
            }
            Word::Mail | Word::Reconnect | Word::Timeout | Word::Exit | Word::Extend => {}
            Word::Approved => {
                rec.phase = "prompt".to_string();
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            Word::Escalated | Word::Cleared => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            Word::Continued
            | Word::Typed
            | Word::Restarted
            | Word::Waiting
            | Word::Skipped
            | Word::Unverified
            | Word::Limited
            | Word::Unproven
            | Word::Dialog
            | Word::Declined
            | Word::Chose
            | Word::Exception
            | Word::Probed
            | Word::Reset
            | Word::Needs => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (first, after) = split_word(tail);
                if let Some(rule) = first.strip_prefix("rule=") {
                    rec.phase = rule.to_string();
                    tail = after;
                }
            }
            Word::Hold => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (door, after) = split_word(tail);
                rec.phase = door.to_string();
                tail = after;
            }
            Word::Occupancy => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (measure, after) = split_word(tail);
                rec.phase = measure.strip_prefix("rule=").unwrap_or(measure).to_string();
                tail = after;
            }
            Word::Probe => {
                let (what, after) = split_word(rest);
                rec.phase = what.to_string();
                tail = after;
                if let Some((seq, after)) = take_num(tail, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
        }
        rec.summary = tail.to_string();
        rec
    }

    /// One JSON object, no newline, keys in the order the module doc lists.
    pub fn to_json(&self) -> String {
        let opt_str = |s: &Option<String>| s.as_deref().map_or("null".to_string(), json_str);
        let opt_num = |n: Option<u64>| n.map_or("null".to_string(), |n| n.to_string());
        let mark = self
            .mark
            .map_or_else(String::new, |m| format!(",\"mark\":\"{m:016x}\""));
        let ended = self
            .ended
            .map_or_else(String::new, |e| format!(",\"ended\":{e}"));
        format!(
            "{{\"t\":{},\"sid\":{},\"kind\":{},\"phase\":{},\"seq\":{},\"complete\":{},\
             \"rows\":{},\"summary\":{},\"line\":{},\"turn\":{},\"report\":{}{mark}{ended}}}",
            self.t,
            opt_str(&self.sid),
            json_str(&self.kind),
            json_str(&self.phase),
            opt_num(self.seq),
            self.complete.map_or("null", |c| if c { "1" } else { "0" }),
            opt_num(self.rows),
            json_str(&self.summary),
            json_str(&self.line),
            opt_num(self.turn),
            opt_num(self.report),
        )
    }

    /// Read one journal line back.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v = Json::parse(text.trim())?;
        let s = |k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
        let n = |k: &str| v.get(k).and_then(Json::as_u64);
        let t = match v.get("t") {
            Some(Json::Number(n)) => n.parse::<i64>().map_err(|_| format!("t={n} is not ms"))?,
            _ => return Err("no \"t\"".to_string()),
        };
        Ok(JournalRecord {
            t,
            sid: s("sid"),
            kind: s("kind").ok_or("no \"kind\"")?,
            phase: s("phase").unwrap_or_else(|| "-".to_string()),
            seq: n("seq"),
            complete: n("complete").map(|c| c == 1),
            rows: n("rows"),
            summary: s("summary").unwrap_or_default(),
            line: s("line").ok_or("no \"line\"")?,
            turn: n("turn"),
            report: n("report"),
            mark: s("mark").and_then(|h| u64::from_str_radix(&h, 16).ok()),
            ended: n("ended").and_then(|e| i64::try_from(e).ok()),
        })
    }
}

/// What an app log may keep of a line a loop printed: the head [`JournalRecord::of_line`] reads
/// (the printed word; an `EVENT`'s or `DISMISSED`'s phase; `seq=`; `complete=`/`rows=` or
/// `report=`/`rows=`; a `rule=`; `HOLD`'s door, `OCCUPANCY`'s measure, `PROBE`'s what), each
/// token kept only where it is a word of `[A-Za-z0-9@._:=-]` (else `?`), then ` text=<n>B`, the
/// free-text tail's length in bytes (none for an empty tail). A line that begins with no [`Word`]
/// is its [`Noted`] word (`NOT ESCALATED`, `WATCHING`, …) or `other`, then the length of the rest.
/// The tail is never read again: for a line the loop printed, the result depends on its tail only
/// through the tail's length.
#[must_use]
pub fn log_words(line: &str) -> String {
    let rec = JournalRecord::of_line(0, None, line, None);
    let (head, tail) = if Word::of_kind(&rec.kind).is_some() {
        // `summary` is a suffix of `line` (each reader step takes a slice's rest).
        let cut = line.len().saturating_sub(rec.summary.len());
        (line.get(..cut).unwrap_or_default(), rec.summary.len())
    } else {
        match Noted::of_line(line) {
            Some(noted) => {
                let rest = line.get(noted.printed().len()..).unwrap_or_default();
                (
                    noted.printed(),
                    rest.strip_prefix(' ').unwrap_or(rest).len(),
                )
            }
            None => ("other", line.len()),
        }
    };
    let mut out = String::with_capacity(head.len().saturating_add(16));
    for token in head.split(' ').filter(|t| !t.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        let word = token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@._:=-".contains(&b));
        out.push_str(if word { token } else { "?" });
    }
    if tail > 0 {
        out.push_str(&format!(" text={tail}B"));
    }
    out
}

/// `s` as a quoted JSON string through the control path's one escape.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    aterm_codec::json::escape_into(&mut out, s);
    out.push('"');
    out
}

/// The current time, Unix milliseconds.
pub fn unix_ms() -> i64 {
    aterm_time::unix_millis().map_or(0, |ms| i64::try_from(ms).unwrap_or(i64::MAX))
}

/// The open journal of one loop (or none).
#[derive(Debug)]
pub struct Journal {
    path: Option<PathBuf>,
    file: Option<std::fs::File>,
    sid: Option<String>,
    /// The one warning was said.
    warned: bool,
}

impl Journal {
    /// No journal: [`Self::record`] does nothing.
    pub const fn off() -> Self {
        Self {
            path: None,
            file: None,
            sid: None,
            warned: false,
        }
    }

    /// A journal at `path` for a loop on `sid` that is named and never opened.
    #[must_use]
    pub fn named(path: Option<&Path>, sid: Option<&str>) -> Self {
        let mut j = Self::off();
        if let Some(path) = path {
            j.path = Some(path.to_path_buf());
            j.sid = sid.map(str::to_string);
        }
        j
    }

    /// Open `path` append-only (created `0600` when missing) for a loop on `sid`.
    pub fn open(path: Option<&Path>, sid: Option<&str>, warn: &mut dyn Write) -> Self {
        let mut j = Self::off();
        let Some(path) = path else {
            return j;
        };
        j.path = Some(path.to_path_buf());
        j.sid = sid.map(str::to_string);
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        match o.open(path) {
            Ok(f) => j.file = Some(f),
            Err(e) => j.failed(&e, "cannot open it", warn),
        }
        j
    }

    /// Appends the record of `line` ([`JournalRecord::of_line`]), stamped `t_ms`. A test build
    /// refuses a line the ledger would read `unread` (`ledger::assert_marked`): every line a
    /// loop journals begins with a [`Word`] or a [`Noted`] word.
    pub fn record(&mut self, t_ms: i64, line: &str, warn: &mut dyn Write) -> Option<String> {
        self.record_marked(t_ms, line, None, warn)
    }

    /// [`Self::record`] of a turn end's line, with the mark of the worker's last words.
    pub fn record_marked(
        &mut self,
        t_ms: i64,
        line: &str,
        mark: Option<u64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.record_with(t_ms, line, mark, None, warn)
    }

    /// [`Self::record`] of a task end's note that restates an end a loop before this one noted.
    pub fn record_ended(
        &mut self,
        t_ms: i64,
        line: &str,
        ended: Option<i64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.record_with(t_ms, line, None, ended, warn)
    }

    /// [`Self::record`] with the record's mark and end time.
    fn record_with(
        &mut self,
        t_ms: i64,
        line: &str,
        mark: Option<u64>,
        ended: Option<i64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        #[cfg(test)]
        super::ledger::assert_marked(line);
        self.path.as_ref()?;
        let mut rec = JournalRecord::of_line(t_ms, self.sid.as_deref(), line, None);
        rec.mark = mark;
        rec.ended = ended;
        let mut text = rec.to_json();
        text.push('\n');
        let res = match self.file.as_mut() {
            Some(f) => f.write_all(text.as_bytes()).and_then(|()| f.flush()),
            None => return Some(text),
        };
        if let Err(e) = res {
            self.failed(&e, "cannot append to it", warn);
        }
        Some(text)
    }

    /// Whether a journal was given (a file named, opened or not).
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.path.is_some()
    }

    /// Append one line that is already a JSON object.
    pub fn append_raw(&mut self, json: &str, warn: &mut dyn Write) -> bool {
        let res = match self.file.as_mut() {
            Some(f) => {
                let mut row = String::with_capacity(json.len() + 1);
                row.push_str(json);
                row.push('\n');
                f.write_all(row.as_bytes()).and_then(|()| f.flush())
            }
            None => return false,
        };
        if let Err(e) = res {
            self.failed(&e, "cannot append to it", warn);
            return false;
        }
        true
    }

    /// A write that failed: warned once ([`Self::warn`]); an out-of-space or quota error also
    /// asks the window's disk watch to look now ([`crate::harness::disk::poke`]).
    fn failed(&mut self, e: &std::io::Error, what: &str, warn: &mut dyn Write) {
        if crate::harness::disk::poke::is_out_of_space(e) {
            let path = self
                .path
                .as_deref()
                .map_or_else(String::new, |p| p.display().to_string());
            crate::harness::disk::poke::poke(crate::harness::disk::poke::Cause::Write(format!(
                "journal {path}: {e}"
            )));
        }
        self.warn(&format!("{what}: {e}"), warn);
    }

    fn warn(&mut self, why: &str, warn: &mut dyn Write) {
        if std::mem::replace(&mut self.warned, true) {
            return;
        }
        let path = self
            .path
            .as_deref()
            .map_or_else(String::new, |p| p.display().to_string());
        let _ = writeln!(
            warn,
            "aterm drive: journal {path}: {why} — the loop goes on without it"
        );
        let _ = warn.flush();
    }
}

/// Every record in a journal file, and how many of its non-blank lines are not one.
pub fn read_journal(path: &Path) -> Result<(Vec<JournalRecord>, usize), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    let mut bad = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match JournalRecord::from_json(line) {
            Ok(r) => out.push(r),
            Err(_) => bad += 1,
        }
    }
    Ok((out, bad))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind of line the loops print, read into its kind, phase, `seq=`
    /// and free-text tail: one row per arm of the reader.
    #[test]
    fn each_printed_line_is_read_into_its_fields() {
        let rec = |line: &str| JournalRecord::of_line(7, Some("@s-1"), line, None);
        for (line, kind, phase, seq, summary) in [
            (
                "EVENT idle seq=106 complete=1 rows=689 ⏺ Done.",
                "event",
                "idle",
                Some(106),
                "⏺ Done.",
            ),
            (
                "EVENT question seq=104 ⏺ Keep it?",
                "event",
                "question",
                Some(104),
                "⏺ Keep it?",
            ),
            (
                "EVENT compacted seq=102",
                "event",
                "compacted",
                Some(102),
                "",
            ),
            (
                "EVENT turn seq=106 report=5 rows=12 ⏺ Done.",
                "event",
                "turn",
                Some(106),
                "⏺ Done.",
            ),
            (
                "APPROVED seq=102 git log --oneline -5",
                "approved",
                "prompt",
                Some(102),
                "git log --oneline -5",
            ),
            (
                "DISMISSED survey seq=31",
                "dismissed",
                "survey",
                Some(31),
                "",
            ),
            (
                "RECONNECT await seq 7 failed: closed",
                "reconnect",
                "-",
                None,
                "await seq 7 failed: closed",
            ),
            (
                "RECONNECTED after 1834 ms",
                "reconnect",
                "-",
                None,
                "after 1834 ms",
            ),
            ("TIMEOUT", "timeout", "-", None, ""),
            ("EXIT session gone", "exit", "-", None, "session gone"),
            (
                "MAIL id=5 off=91 kind=report",
                "mail",
                "-",
                None,
                "id=5 off=91 kind=report",
            ),
            (
                "EXTEND until=2026-09-19T18:10:00Z",
                "extend",
                "-",
                None,
                "until=2026-09-19T18:10:00Z",
            ),
            (
                "ESCALATED seq=102 attention=OK",
                "escalated",
                "-",
                Some(102),
                "attention=OK",
            ),
            (
                "CLEARED seq=91 exception=spend",
                "cleared",
                "-",
                Some(91),
                "exception=spend",
            ),
            (
                "EXCEPTION seq=88 kind=spend when=later x",
                "exception",
                "-",
                Some(88),
                "kind=spend when=later x",
            ),
            (
                "CONTINUED seq=102 rule=usage-resume@v1 keep going",
                "continued",
                "usage-resume@v1",
                Some(102),
                "keep going",
            ),
            (
                "RESET seq=41 rule=limit-reset@v1 used (1 left)",
                "reset",
                "limit-reset@v1",
                Some(41),
                "used (1 left)",
            ),
            (
                "WAITING seq=9 until=2026-09-19T18:10:00Z overloaded",
                "waiting",
                "-",
                Some(9),
                "until=2026-09-19T18:10:00Z overloaded",
            ),
            (
                "HOLD seq=9 point nothing why=withheld",
                "hold",
                "point",
                Some(9),
                "nothing why=withheld",
            ),
            (
                "HOLD seq=12 background held why=act-untaken",
                "hold",
                "background",
                Some(12),
                "held why=act-untaken",
            ),
            (
                "OCCUPANCY seq=14 hold closed why=foreign held_s=7260",
                "occupancy",
                "hold",
                Some(14),
                "closed why=foreign held_s=7260",
            ),
            (
                "OCCUPANCY seq=15 rule=wait-nudge@v1 rev=builtin episode=00ff door=break would",
                "occupancy",
                "wait-nudge@v1",
                Some(15),
                "rev=builtin episode=00ff door=break would",
            ),
            (
                "PROBE deferred seq=102 text is typed",
                "probe",
                "deferred",
                Some(102),
                "text is typed",
            ),
            (
                "PROBE skipped gate=not-live",
                "probe",
                "skipped",
                None,
                "gate=not-live",
            ),
            (
                "PROBING seq=102 rule=usage-probe@v1 /usage",
                "other",
                "-",
                None,
                "PROBING seq=102 rule=usage-probe@v1 /usage",
            ),
            (
                "reset seq=41 rule=limit-reset@v1 unread",
                "other",
                "-",
                None,
                "reset seq=41 rule=limit-reset@v1 unread",
            ),
        ] {
            let r = rec(line);
            assert_eq!(
                (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
                (kind, phase, seq, summary),
                "{line}"
            );
            assert_eq!(r.sid.as_deref(), Some("s-1"));
        }
        let r = rec("EVENT idle seq=106 complete=1 rows=689 ⏺ Done.");
        assert_eq!(
            (r.complete, r.rows, r.report),
            (Some(true), Some(689), None)
        );
        let r = rec("EVENT turn seq=106 report=5 rows=12 ⏺ Done.");
        assert_eq!((r.complete, r.rows, r.report), (None, Some(12), Some(5)));
        for word in [
            "TYPED",
            "RESTARTED",
            "UNVERIFIED",
            "LIMITED",
            "UNPROVEN",
            "DIALOG",
            "DECLINED",
            "CHOSE",
            "PROBED",
        ] {
            let r = rec(&format!("{word} seq=5 rule=a-rule@v1 x"));
            assert_eq!(
                (r.kind, r.phase.as_str(), r.seq, r.summary.as_str()),
                (word.to_ascii_lowercase(), "a-rule@v1", Some(5), "x"),
                "{word}"
            );
        }
        for gate in crate::supervise::policy::turn_end::ProbeGate::ALL {
            let line = format!("SKIPPED seq=7 rule=usage-probe@v1 gate={}", gate.word());
            let r = rec(&line);
            assert_eq!(
                (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary),
                (
                    "skipped",
                    "usage-probe@v1",
                    Some(7),
                    format!("gate={}", gate.word())
                ),
                "{line}"
            );
        }
    }

    /// Each [`Word`] is the kind of a line that begins with its printed form and of nothing
    /// else, and its name reads back to it; a lower-case word or an unknown one is `other`.
    #[test]
    fn every_word_is_read_by_its_own_name() {
        let mut names: Vec<&str> = Word::ALL.iter().map(|w| w.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Word::ALL.len(), "two words share a name");
        for w in Word::ALL {
            let printed = w.printed();
            assert_eq!(Word::of_printed(&printed), Some(w), "{printed}");
            assert_eq!(Word::of_kind(w.name()), Some(w), "{w:?}");
            let r = JournalRecord::of_line(1, None, &format!("{printed} seq=1 x"), None);
            assert_eq!(r.kind, w.name(), "{printed}");
            assert_eq!(Word::of_printed(w.name()), None, "{w:?} in lower case");
        }
        assert_eq!(Word::Needs.printed(), NEEDS);
        assert_eq!(Word::of_printed("RECONNECTED"), Some(Word::Reconnect));
        for other in ["FROZEN", "ADOPTED", "PROBING", "", "Event"] {
            assert_eq!(Word::of_printed(other), None, "{other}");
            let r = JournalRecord::of_line(1, None, &format!("{other} seq=1 x"), None);
            assert_eq!(r.kind, "other", "{other}");
        }
        assert_eq!(Word::of_kind("other"), None);
    }

    /// [`log_words`] keeps each line's head as [`JournalRecord::of_line`] reads it and its
    /// tail's length, never the tail: a tail holding a planted word and an equal-length tail of
    /// other bytes give the same words, for every [`Word`] (each word's line shape is this
    /// match's, so a new word does not compile until the test names it), every [`Noted`] word,
    /// the shapes a real Claude Code run logged, and a line of no word.
    #[test]
    fn log_words_keeps_each_words_head_and_never_its_text() {
        const PLANT: &str = "QZP7WORD5";
        // The head the loop prints before a line's free text, and whether its grammar reads a
        // `seq=` there.
        let shape = |w: Word| -> (&'static str, bool) {
            match w {
                Word::Event => ("EVENT idle seq=7", true),
                Word::Dismissed => ("DISMISSED survey seq=7", true),
                Word::Mail => ("MAIL", false),
                Word::Approved => ("APPROVED seq=7", true),
                Word::Reconnect => ("RECONNECT", false),
                Word::Timeout => ("TIMEOUT", false),
                Word::Exit => ("EXIT", false),
                Word::Extend => ("EXTEND", false),
                Word::Escalated => ("ESCALATED seq=7", true),
                Word::Cleared => ("CLEARED seq=7", true),
                Word::Continued => ("CONTINUED seq=7 rule=usage-resume@v1", true),
                Word::Typed => ("TYPED seq=7 rule=a-rule@v1", true),
                Word::Restarted => ("RESTARTED seq=7 rule=a-rule@v1", true),
                Word::Waiting => ("WAITING seq=7", true),
                Word::Skipped => ("SKIPPED seq=7 rule=usage-probe@v1", true),
                Word::Unverified => ("UNVERIFIED seq=7 rule=a-rule@v1", true),
                Word::Limited => ("LIMITED seq=7 rule=a-rule@v1", true),
                Word::Unproven => ("UNPROVEN seq=7 rule=a-rule@v1", true),
                Word::Dialog => ("DIALOG seq=7 rule=a-rule@v1", true),
                Word::Declined => ("DECLINED seq=7 rule=a-rule@v1", true),
                Word::Chose => ("CHOSE seq=7 rule=answer-recommended@v1", true),
                Word::Exception => ("EXCEPTION seq=7", true),
                Word::Probed => ("PROBED seq=7 rule=usage-probe@v1", true),
                Word::Reset => ("RESET seq=7 rule=limit-reset@v1", true),
                Word::Needs => ("NEEDS seq=7", true),
                Word::Hold => ("HOLD seq=7 point", true),
                Word::Occupancy => ("OCCUPANCY seq=7 rule=wait-nudge@v1", true),
                Word::Probe => ("PROBE deferred seq=7", true),
            }
        };
        let mut rows: Vec<(String, bool)> = Word::ALL
            .into_iter()
            .map(|w| {
                let (head, seq) = shape(w);
                (head.to_string(), seq)
            })
            .collect();
        rows.extend(Noted::ALL.map(|n| (n.printed().to_string(), false)));
        // The real run's shapes (a reply's last line, a box's classification and command, its
        // approval, a question's answer), each head read to the end of what the grammar reads.
        rows.extend(
            [
                "EVENT idle seq=7 complete=1 rows=689",
                "EVENT question seq=7",
                "EVENT prompt seq=7",
                "EVENT turn seq=7 report=5 rows=12",
                "APPROVED seq=7",
                "CHOSE seq=7 rule=answer-recommended@v1",
            ]
            .map(|h| (h.to_string(), true)),
        );
        let tails = [
            format!("⏺ {PLANT} is the reply's last line"),
            format!("kind=bash classify=not-read-only:touch command=touch {PLANT}"),
            format!("touch {PLANT}"),
            format!("policy=recommended {PLANT} (Recommended)"),
            PLANT.to_string(),
        ];
        for (head, seq) in &rows {
            for tail in &tails {
                let line = format!("{head} {tail}");
                let other = format!("{head} {}", "x".repeat(tail.len()));
                let words = log_words(&line);
                assert_eq!(
                    words,
                    log_words(&other),
                    "{line}: the tail's bytes reach it"
                );
                assert_eq!(
                    words,
                    format!("{head} text={}B", tail.len()),
                    "{line}: its head and the tail's length"
                );
                assert!(!words.contains("QZP7WORD"), "{words}");
                assert_eq!(words.contains(" seq=7 "), *seq, "{words}");
            }
            assert_eq!(&log_words(head), head, "an empty tail has no text=");
        }
        let word_less = format!("PROBING seq=102 rule=usage-probe@v1 /usage {PLANT}");
        assert_eq!(
            log_words(&word_less),
            format!("other text={}B", word_less.len()),
            "a line of no word is `other` and its length"
        );
        assert_eq!(log_words(""), "other");
        assert_eq!(
            log_words(&format!("EVENT ⏺{PLANT} seq=7 x")),
            "EVENT ? seq=7 text=1B",
            "a head token that is no word is `?`"
        );
    }

    /// The JSON round-trips, escapes what it must, and prints `null` for what
    /// the line did not say.
    #[test]
    fn a_record_round_trips_through_its_json() {
        let r = JournalRecord::of_line(
            1_789_407_079_123,
            Some("@s-1e91"),
            "EVENT idle seq=5 complete=0 rows=3 ⏺ \"quoted\" \\ tab\t",
            Some(8),
        );
        let json = r.to_json();
        assert!(
            json.starts_with(
                "{\"t\":1789407079123,\"sid\":\"s-1e91\",\"kind\":\"event\",\"phase\":\"idle\",\
             \"seq\":5,\"complete\":0,\"rows\":3,\"summary\":\"⏺ \\\"quoted\\\" \\\\ tab\\t\""
            ),
            "{json}"
        );
        assert!(json.ends_with(",\"turn\":8,\"report\":null}"), "{json}");
        assert_eq!(JournalRecord::from_json(&json), Ok(r));
        let t = JournalRecord::of_line(1, None, "TIMEOUT", None).to_json();
        assert_eq!(
            t,
            "{\"t\":1,\"sid\":null,\"kind\":\"timeout\",\"phase\":\"-\",\"seq\":null,\
             \"complete\":null,\"rows\":null,\"summary\":\"\",\"line\":\"TIMEOUT\",\"turn\":null,\
             \"report\":null}"
        );
        let r = JournalRecord::of_line(2, Some("s-1"), "EVENT turn seq=9 report=5 rows=3 x", None);
        let json = r.to_json();
        assert!(json.ends_with(",\"turn\":null,\"report\":5}"), "{json}");
        assert_eq!(JournalRecord::from_json(&json), Ok(r));
        let old = JournalRecord::from_json(
            "{\"t\":1,\"kind\":\"event\",\"phase\":\"idle\",\"line\":\"EVENT idle seq=1 x\"}",
        )
        .expect("an older record");
        assert_eq!((old.report, old.turn, old.mark), (None, None, None));
        let mut r =
            JournalRecord::of_line(3, Some("s-1"), "EVENT idle seq=4 ATERM-TASK-DONE", None);
        r.mark = Some(0xfeed_0000_0000_beef);
        let json = r.to_json();
        assert!(
            json.ends_with(",\"report\":null,\"mark\":\"feed00000000beef\"}"),
            "{json}"
        );
        assert_eq!(JournalRecord::from_json(&json), Ok(r));
        let mut r = JournalRecord::of_line(
            5,
            Some("s-1"),
            "DONE seq=4 rule=finished@v1 the worker reported ATERM-TASK-DONE",
            None,
        );
        assert!(!r.to_json().contains("ended"), "{}", r.to_json());
        r.ended = Some(1_792_458_186_177);
        let json = r.to_json();
        assert!(
            json.ends_with(",\"report\":null,\"ended\":1792458186177}"),
            "{json}"
        );
        assert_eq!(JournalRecord::from_json(&json), Ok(r));
        assert_eq!(old.ended, None);
        assert!(JournalRecord::from_json("{\"kind\":\"event\"}").is_err());
        assert!(JournalRecord::from_json("{\"t\":1,\"kind\":\"event\",\"line\":").is_err());
    }

    /// A journal write that fails with `ENOSPC` or `EDQUOT` pokes the disk watch naming the
    /// journal; any other failure pokes nothing.
    #[test]
    fn a_journal_write_out_of_space_pokes_the_disk_watch() {
        use crate::harness::disk::poke::{Cause, set_handler};
        use std::io::{Error, ErrorKind};
        use std::sync::{Arc, Mutex, PoisonError};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        set_handler(Arc::new(|cause| {
            if let Cause::Write(w) = cause {
                HEARD
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(w.clone());
            }
        }));
        let named = |tag: &str| {
            HEARD
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|w| w.contains(tag))
        };
        let denied = format!("journal-denied-{}", std::process::id());
        let mut j = Journal::off();
        j.path = Some(PathBuf::from(&denied));
        let mut warn = Vec::new();
        j.failed(
            &Error::from(ErrorKind::PermissionDenied),
            "cannot append to it",
            &mut warn,
        );
        assert!(!named(&denied), "no full disk, no poke");
        assert!(!warn.is_empty(), "said");
        for kind in [ErrorKind::StorageFull, ErrorKind::QuotaExceeded] {
            let full = format!("journal-full-{kind:?}-{}", std::process::id());
            let mut j = Journal::off();
            j.path = Some(PathBuf::from(&full));
            j.failed(&Error::from(kind), "cannot append to it", &mut Vec::new());
            assert!(named(&full), "{kind:?} pokes the watch");
        }
    }
}
