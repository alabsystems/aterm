// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervising loop's own record of what it decided
//! ([`super::SuperviseOpts::journal`], the file its window host names), one JSON object per
//! line, which `aterm drive ledger` replays.
//!
//! Every line the loop prints is journaled as printed, and so is every journal-only line it
//! decides silently, as
//!
//! ```text
//! {"t":<unix ms>,"sid":"<sid>"|null,
//!  "kind":"event|approved|declined|dismissed|reconnect|timeout|exit|mail|extend|escalated|
//!          cleared|continued|typed|restarted|waiting|skipped|unverified|limited|unproven|
//!          dialog|chose|exception|probed|reset|probe|needs|hold|occupancy",
//!  "phase":"idle|question|prompt|limited|survey|context|compacted|turn|idle-no-report|
//!           <rule id>|resumed|still-limited|rebriefed|rebrief-failed|point|background|-",
//!  "seq":<n>|null,"complete":0|1|null,"rows":<n>|null,"summary":"<the line's free-text tail>",
//!  "line":"<the exact line>","turn":<id>|null,"report":<id>|null[,"mark":"<16 hex>"]
//!  [,"ended":<unix ms>]}
//! ```
//!
//! read from the line itself ([`JournalRecord::of_line`]), so record and line cannot disagree.
//! An `EVENT`'s phase is its second word; a policy line's is its `rule=<id>` where it names
//! one. `complete`, `rows`, `turn` and `report` are an older build's, read back from its
//! journals and written `null` now: the `complete=<0|1> rows=<n>` of a removed `watch
//! --report`, with the ledger turn its report was counted from, and the inbox row id of the
//! report a removed `watch --mail` folded into an `EVENT turn` line; a `MAIL …` line is `kind`
//! `mail`, its words the summary. `mark`, on a review point's `EVENT` alone and only where the
//! worker's last words were read there, is their mark (`policy::turn_end::said_mark`): what
//! the next loop compares a landing's point with (`run.rs`'s `ended_before`), never printed.
//! `ended`, on a task end's `DONE` note a restarted loop wrote for the end a loop before it
//! noted, is when that end was first noted (unix ms), carried forward by each restatement:
//! the time the next loop restates the end with (`ended_before`), never printed.
//!
//! Some lines are journal-only, never printed (`Review::note`): `ESCALATED`, `CLEARED`,
//! `NEEDS` (the worker's NEEDS YOU put on the person's screen: [`NEEDS`]),
//! `WAITING`, `HOLD` (where the loop typed nothing though the agent stood still, and why: a
//! `point` decided nothing, a `background` break no host step was offered at; its door is
//! its phase), `OCCUPANCY` (what the occupancy measures of the agent's wait, once a change:
//! `hold open|declared|closed`, its phase the measure, `hold`; and what a built-in rule would
//! do at a break, `rule=<id> rev=builtin episode=<hex> door=break
//! would|held:<guard>|paced next_s=<n>|spent`, its phase the rule; `supervise::occupancy`),
//! `UNVERIFIED`, most `SKIPPED`, `UNPROVEN` (after a full-power `APPROVED`: why no
//! safe rule proved the box), `DIALOG` (a question decision's rows joined by ` ⏎ `),
//! `DECLINED`, `PROBED … /usage no sample (<word>)`, and every `RESET` but `used (<n> left)`.
//! A due usage probe not run is the printed `SKIPPED seq=<n> rule=usage-probe@v1
//! gate=<word>[ host=<word>]` (`ProbeGate`'s words). `PROBE <sent|deferred> seq=<n> …` is an
//! older build's limit probe, still read as `probe`.
//!
//! The file is opened append-only, created `0600` when missing; a failure to open or write it
//! is said once on stderr and never stops the loop.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::screen::Json;

// THE WORDS A READER MATCHES ON. The loop's lines below are written through
// these consts, and `harness::stats` reads the person's waits back through
// the same ones, so a line reworded on one side cannot leave the other
// counting nothing.

/// The line that adopts a badge a previous loop left on the session, at this
/// loop's start (`lifecycle.rs`'s `reconcile`): `ADOPTED seq=<n>
/// attention=<text>` — its point still shows, so the badge is this loop's
/// from here, and no second one goes up for it. A record of kind `other`.
pub const ADOPTED: &str = "ADOPTED";

/// The badge a `CLEARED seq=<n> box attention=<reply>` line ended: a box's,
/// a question's or an idle point's the loop escalated (`escalate.rs`'s
/// `close_box`).
pub const CLEARED_BOX: &str = "box";
/// … `CLEARED seq=<n> turn-end attention=<reply>`: the turn-end policy's,
/// raised with no point to carry it (`/login` typed, a switch's note), gone
/// once the worker works again.
pub const CLEARED_TURN_END: &str = "turn-end";
/// … `CLEARED seq=<n> goal attention=<reply>`: the note that Codex's goal
/// runs on under a save-then-wait switch, written by older loops (the Codex
/// harness is retired) and still read in their journals.
pub const CLEARED_GOAL: &str = "goal";
/// … `CLEARED seq=<n> memory attention=<reply>: …`: the memory measure's,
/// no longer acted on.
pub const CLEARED_MEMORY: &str = "memory";
/// … `CLEARED seq=<n> stale attention=<reply> (…)`: one a previous loop left
/// whose point is gone, unset at this loop's start.
pub const CLEARED_STALE: &str = "stale";
/// … `CLEARED seq=- hidden attention=<reply> (…)`: one a previous loop may
/// have left under another owner's entry, unset unread at this loop's start
/// (its `seq=` is `-`: no read was made).
pub const CLEARED_HIDDEN: &str = "hidden";
/// The line that puts THE WORKER'S NEEDS YOU on the person's screen
/// (`escalate.rs`'s `sync_needs`, the `needs` owner's entry): `NEEDS
/// seq=<n> count=<k> attention=<reply> <first need, clipped>`, once a set.
/// No escalation: nothing is handed over and nothing waits, so the
/// harness's stats count it as told ([`crate::harness::stats`]'s
/// `needs_told`), never as a person's wait. A record of kind `needs`.
pub const NEEDS: &str = "NEEDS";
/// … `CLEARED seq=<n> needs attention=<reply> <why>`: the needs entry taken
/// down (none stands now, an Off park, the loop's end, or — one a loop
/// before this one left — no need stands at the loop's start). No badge of the
/// loop's escalations ([`CLEARED_BADGES`] does not hold it): it closes no
/// person's wait.
pub const CLEARED_NEEDS: &str = "needs";

/// Every word a `CLEARED` line names a badge by. A limit episode's names
/// none: `CLEARED seq=<n> attention=<reply> <why>`.
pub const CLEARED_BADGES: [&str; 6] = [
    CLEARED_BOX,
    CLEARED_TURN_END,
    CLEARED_GOAL,
    CLEARED_MEMORY,
    CLEARED_STALE,
    CLEARED_HIDDEN,
];

/// The `<reply>` a badge's `CLEARED` line names where the loop's entry was
/// NOT taken down: the server keeps one entry per owner, and it now carries
/// another of this loop's escalations (`escalate.rs`'s `unset_if_ours`) —
/// the badge it named is over, the loop's still stands.
pub const ATTENTION_KEPT: &str = "kept";

/// The line a stall's hold writes (`stall.rs`'s `hold_for_stall`): `FROZEN
/// seq=<n> input=<word> wait_ms=<n> bytes=<n> rss_mb=<n> attention=<reply>`.
/// A record of kind `other`. The hold takes the loop's point badge down (the
/// server's own entry says "frozen" over it), and no `CLEARED` line follows:
/// its `attention=<reply>` is that unset's — [`ATTENTION_KEPT`] where the
/// entry carries a limit's badge, which stands, and [`ATTENTION_SKIPPED`]'s
/// `skipped: …` behind another supervisor, which touched nothing. Any other
/// reply took the loop's entry down (or found none).
pub const FROZEN: &str = "FROZEN";

/// How an `ESCALATED seq=<n> attention=<reply>` line's tail begins where NO
/// BADGE went up, with why after it: another supervisor holds the session
/// ([`ESCALATED_BEHIND`]), or a stall was in hand and the ask was withheld
/// (`stall.rs`). Nobody was asked by this loop.
pub const ATTENTION_SKIPPED: &str = "attention=skipped: ";
/// Why [`ATTENTION_SKIPPED`] behind another supervisor's claim: `… another
/// supervisor (<holder>) answers this session`.
pub const ESCALATED_BEHIND: &str = "another supervisor";
/// The `again=<why>` an `ESCALATED` line ends with where its badge is the
/// same ask set again, no new one — the harness's stats count no second
/// escalation. Here a limit episode's, where a retry hit the wall again
/// before the worker answered, or the notice an Autopilot Off park took the
/// badge off is still up after it (`run.rs`'s `open_episode`): `again=the
/// retry hit the wall again, the episode of seq=<n>`. (A journal written
/// before the manager's mail went says these as `mail=skipped: <why>`.)
pub const AGAIN_RETRY: &str = "the retry hit the wall again";
/// The `again=<why>` of a limit episode's escalation over a badge adopted
/// from a previous loop ([`ADOPTED`]): the same ask, set again. Only while
/// the notice it was adopted for still stands: once the worker answers past
/// it, a limit is a new episode, a new ask.
pub const AGAIN_ADOPTED: &str = "adopted from a previous watcher";
/// The `again=<why>` of the turn-end policy's badge set again as an
/// Autopilot Off park ends (`lifecycle.rs`'s `raise_parked_badge`): the
/// badge the park took down, up again with no second ask of its own. Most
/// such badges no point raises again (`/login`'s); one an act's own miss
/// raised (a text not written or left typed, a draft with no fenced Enter)
/// may be escalated again after this line, by its act decided afresh and
/// missing again — its own `ESCALATED` line and ask.
pub const AGAIN_UNPARKED: &str = "set again after an off park";

/// One journaled line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    /// When it was printed, Unix milliseconds.
    pub t: i64,
    /// The session the loop watched (the `@sid` it was given, without the
    /// `@`), `None` when it was given none.
    pub sid: Option<String>,
    /// The line's leading word lowercased (`RECONNECTED` is `reconnect`), `other` for a word
    /// the reader does not know.
    pub kind: String,
    /// The phase the line is about (an `EVENT`'s second word, `prompt` for an `APPROVED`) or a
    /// policy line's `rule=` id; `-` for none.
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
    /// The mark of the worker's last words at a review point
    /// (`policy::turn_end::said_mark`), `None` on every other line and where
    /// no words were read.
    pub mark: Option<u64>,
    /// THE TIME THE END THIS NOTE RESTATES WAS FIRST NOTED (unix ms), on a
    /// task end's note (`DONE …`) a restarted loop wrote for the end a loop
    /// before it noted — a landing seeded and taken up at its first point
    /// (`policy::turn_end::TurnEndState::restated_noted_ms`) — carried
    /// forward by every restatement, so the end's time survives any number
    /// of restarts and seamless updates (`run.rs`'s `ended_before`); `None`
    /// on every other line, and on a note of an end of its own.
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
        let mut tail = rest;
        match word {
            "EVENT" | "DISMISSED" => {
                rec.kind = if word == "EVENT" {
                    "event"
                } else {
                    "dismissed"
                }
                .to_string();
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
            "MAIL" => rec.kind = "mail".to_string(),
            "APPROVED" => {
                rec.kind = "approved".to_string();
                rec.phase = "prompt".to_string();
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            "RECONNECT" | "RECONNECTED" => rec.kind = "reconnect".to_string(),
            "TIMEOUT" => rec.kind = "timeout".to_string(),
            "EXIT" => rec.kind = "exit".to_string(),
            "EXTEND" => rec.kind = "extend".to_string(),
            "ESCALATED" | "CLEARED" => {
                rec.kind = word.to_ascii_lowercase();
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            "CONTINUED" | "TYPED" | "RESTARTED" | "WAITING" | "SKIPPED" | "UNVERIFIED"
            | "LIMITED" | "UNPROVEN" | "DIALOG" | "DECLINED" | "CHOSE" | "EXCEPTION" | "PROBED"
            | "RESET" | NEEDS => {
                rec.kind = word.to_ascii_lowercase();
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
            "HOLD" => {
                rec.kind = "hold".to_string();
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (door, after) = split_word(tail);
                rec.phase = door.to_string();
                tail = after;
            }
            "OCCUPANCY" => {
                rec.kind = "occupancy".to_string();
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                // A rule's line names its rule; the hold's, its measure.
                let (measure, after) = split_word(tail);
                rec.phase = measure.strip_prefix("rule=").unwrap_or(measure).to_string();
                tail = after;
            }
            "PROBE" => {
                rec.kind = "probe".to_string();
                let (what, after) = split_word(rest);
                rec.phase = what.to_string();
                tail = after;
                if let Some((seq, after)) = take_num(tail, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            _ => tail = line,
        }
        rec.summary = tail.to_string();
        rec
    }

    /// One JSON object, no newline, keys in the order the module doc lists
    /// (`mark` and `ended` only where there is one).
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

    /// Read one journal line back. `t`, `kind` and `line` are required; every
    /// other key may be absent or `null`.
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

/// `s` as a quoted JSON string through the control path's one escape
/// ([`aterm_codec::json::escape_into`]): RFC 8259's escapes, plus every other character a
/// terminal acts on (DEL, C1, the bidi controls, U+2028/9) as `\uXXXX`, since a journal line
/// carries a screen's rows (`DIALOG`) and a person may read the file raw.
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
    pub fn off() -> Self {
        Self {
            path: None,
            file: None,
            sid: None,
            warned: false,
        }
    }

    /// A journal at `path` for a loop on `sid` that is NAMED and never
    /// opened: [`Self::record`] returns each row it would write and writes
    /// none, and nothing is said. What a REPLAY's loop keeps
    /// (`supervise::tape::replay`): its rows are outputs it compares with the
    /// recorded ones, and the directory the path names does not exist.
    #[must_use]
    pub fn named(path: Option<&Path>, sid: Option<&str>) -> Self {
        let mut j = Self::off();
        if let Some(path) = path {
            j.path = Some(path.to_path_buf());
            j.sid = sid.map(str::to_string);
        }
        j
    }

    /// Open `path` append-only (created `0600` when missing) for a loop on
    /// `sid`; no path is no journal. A failure to open it is said once, to
    /// `warn`, and the loop goes on without one.
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

    /// Append the record of `line` (see [`JournalRecord::of_line`]), stamped
    /// `t_ms` (Unix milliseconds, from the caller's clock: the supervising
    /// loop's stamp clock, `Fx::stamp_ms` behind its crate-private clock door
    /// `supervise::fx`; [`unix_ms`] elsewhere). A write that
    /// fails is said once, to `warn`; the next line is tried all the same.
    /// Returns the row it wrote (its newline included) — or would have
    /// written, to a file it could not open — for a journal it was given;
    /// `None` with none: what a harness tape holds of the journal.
    pub fn record(&mut self, t_ms: i64, line: &str, warn: &mut dyn Write) -> Option<String> {
        self.record_marked(t_ms, line, None, warn)
    }

    /// [`Self::record`] of a review point's line, with the mark of the
    /// worker's last words there ([`JournalRecord::mark`]).
    pub fn record_marked(
        &mut self,
        t_ms: i64,
        line: &str,
        mark: Option<u64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.record_with(t_ms, line, mark, None, warn)
    }

    /// [`Self::record`] of a task end's note that restates an end a loop
    /// before this one noted, with the time that end was first noted
    /// ([`JournalRecord::ended`]).
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
    pub fn is_on(&self) -> bool {
        self.path.is_some()
    }

    /// Append one line that is already a JSON object (the approval ledger's rows,
    /// [`super::approvals`]), warning once like [`Self::record`]. `false` with no journal or when
    /// the write failed, for a caller that must not act on what it could not record (the disk
    /// journal's intent rows).
    pub fn append_raw(&mut self, json: &str, warn: &mut dyn Write) -> bool {
        let res = match self.file.as_mut() {
            // The row and its newline in one write call, as `record_with`
            // writes its own: one syscall a row, not two, and no other
            // appender's row can land between a row and its end.
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

    /// A write that failed: said once ([`Self::warn`]), and when the file
    /// system is out of space or quota, the window's disk watch is asked to
    /// look now ([`crate::harness::disk::poke`]) — aterm's own failing write
    /// is a sighting of the full disk like an agent's screen.
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

/// Every record in a journal file, and how many of its non-blank lines are
/// not one (a line cut short by a crash, a hand edit).
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
    /// and free-text tail: one row per arm of the reader. A `rule=` is the
    /// phase; a word nothing here knows (or a lowercase one) is `other`, its
    /// line whole.
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
        // Every word the loops print with a `rule=` is its own kind, the rule
        // its phase: a word dropped from the reader would read as `other`.
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
        // Every gate's word reads as the probe's `skipped`.
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
        // A journal written before `report` existed reads back without it.
        let old = JournalRecord::from_json(
            "{\"t\":1,\"kind\":\"event\",\"phase\":\"idle\",\"line\":\"EVENT idle seq=1 x\"}",
        )
        .expect("an older record");
        assert_eq!((old.report, old.turn, old.mark), (None, None, None));
        // A point's mark: written only where there is one, read back whole
        // (16 hex digits, past what a JSON number holds exactly).
        let mut r =
            JournalRecord::of_line(3, Some("s-1"), "EVENT idle seq=4 ATERM-TASK-DONE", None);
        r.mark = Some(0xfeed_0000_0000_beef);
        let json = r.to_json();
        assert!(
            json.ends_with(",\"report\":null,\"mark\":\"feed00000000beef\"}"),
            "{json}"
        );
        assert_eq!(JournalRecord::from_json(&json), Ok(r));
        // A restated end's first time: written only where there is one, and
        // an older record reads back without it.
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

    /// AN OUT-OF-SPACE WRITE ASKS THE DISK WATCH TO LOOK: a journal write
    /// that fails with `ENOSPC` or `EDQUOT` pokes the watch
    /// ([`crate::harness::disk::poke`]) naming the journal, as the watch's
    /// handler hears it; NEGATIVE CONTROL: any other failure (a permission
    /// denied) pokes nothing. Each failure is still said once.
    #[test]
    fn a_journal_write_out_of_space_pokes_the_disk_watch() {
        use crate::harness::disk::poke::{Cause, set_handler};
        use std::io::{Error, ErrorKind};
        use std::sync::{Arc, Mutex, PoisonError};
        // The one handler, as the window's watch registers it; every poke of
        // this test process reaches it, so each case is told by its tag.
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
