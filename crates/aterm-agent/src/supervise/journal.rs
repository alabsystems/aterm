// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `watch --journal FILE` / `supervise --journal FILE`: the loop's own record of what it
//! decided, one JSON object per line, which `aterm drive ledger` replays.
//!
//! Every line either loop prints is journaled as printed (`watch`'s stdout, `supervise`'s
//! stderr, and for `supervise` the lines `watch` would have printed for what it decides
//! silently), as
//!
//! ```text
//! {"t":<unix ms>,"sid":"<sid>"|null,
//!  "kind":"event|approved|declined|dismissed|reconnect|timeout|exit|mail|extend|escalated|
//!          cleared|continued|typed|restarted|waiting|skipped|unverified|limited|unproven|
//!          dialog|chose|exception|probed|reset|probe",
//!  "phase":"idle|question|prompt|limited|survey|context|compacted|turn|idle-no-report|
//!           <rule id>|resumed|still-limited|rebriefed|rebrief-failed|-",
//!  "seq":<n>|null,"complete":0|1|null,"rows":<n>|null,"summary":"<the line's free-text tail>",
//!  "line":"<the exact line>","turn":<id>|null,"report":<id>|null}
//! ```
//!
//! read from the line itself ([`JournalRecord::of_line`]), so record and line cannot disagree.
//! An `EVENT`'s phase is its second word; a policy line's is its `rule=<id>` where it names
//! one. `turn` is the ledger turn the point's report was counted from (`watch --report`);
//! `report` is the inbox row id of the report `watch --mail` folded into an `EVENT turn`
//! line, whose `rows=` is that body's row count.
//!
//! Some lines are journal-only, never printed (`Review::note`): `ESCALATED`, `CLEARED`,
//! `WAITING`, `UNVERIFIED`, most `SKIPPED`, `UNPROVEN` (after a full-power `APPROVED`: why no
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
/// runs on under a save-then-wait switch.
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
/// seq=<n> input=<word> wait_ms=<n> bytes=<n> rss_mb=<n> attention=<reply>
/// mail=<reply>`. A record of kind `other`. The hold takes the loop's point
/// badge down (the server's own entry says "frozen" over it), and no
/// `CLEARED` line follows: its `attention=<reply>` is that unset's —
/// [`ATTENTION_KEPT`] where the entry carries a limit's badge, which stands,
/// and [`ATTENTION_SKIPPED`]'s `skipped: …` behind another supervisor, which
/// touched nothing. Any other reply took the loop's entry down (or found
/// none).
pub const FROZEN: &str = "FROZEN";

/// How an `ESCALATED seq=<n> attention=<reply> mail=<reply>` line's tail
/// begins where NO BADGE went up, with why after it: another supervisor
/// holds the session ([`ESCALATED_BEHIND`]), or a stall was in hand and the
/// ask was withheld (`stall.rs`). Nobody was asked by this loop.
pub const ATTENTION_SKIPPED: &str = "attention=skipped: ";
/// Why [`ATTENTION_SKIPPED`] behind another supervisor's claim: `… another
/// supervisor (<holder>) answers this session`.
pub const ESCALATED_BEHIND: &str = "another supervisor";
/// The mail word of a limit episode's escalation where a retry hit the wall
/// again before the worker answered, or the notice an Autopilot Off park
/// took the badge off is still up after it (`run.rs`'s `open_episode`): the
/// same episode's badge up again, no second ask — `mail=skipped: the retry
/// hit the wall again, the episode of seq=<n>`.
pub const MAIL_RETRY: &str = "skipped: the retry hit the wall again";
/// The mail word of a limit episode's escalation over a badge adopted from a
/// previous loop ([`ADOPTED`]): the same ask, set again — `mail=skipped:
/// adopted from a previous watcher`. Only while the notice it was adopted
/// for still stands: once the worker answers past it, a limit is a new
/// episode, mailed.
pub const MAIL_ADOPTED: &str = "skipped: adopted from a previous watcher";
/// The mail word of the turn-end policy's badge set again as an Autopilot
/// Off park ends (`lifecycle.rs`'s `raise_parked_badge`): the badge the park
/// took down, up again with no second ask of its own — `mail=skipped: set
/// again after an off park`. Most such badges no point raises again
/// (`/login`'s, a switch's note, a sandbox fall); one an act's own miss
/// raised (a text not written or left typed, a draft with no fenced Enter)
/// may be escalated again after this line, by its act decided afresh and
/// missing again — its own `ESCALATED` line and ask.
pub const MAIL_UNPARKED: &str = "skipped: set again after an off park";

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
    /// The line's `complete=` (`watch --report`).
    pub complete: Option<bool>,
    /// The line's `rows=` (`watch --report`).
    pub rows: Option<u64>,
    /// The line's free-text tail.
    pub summary: String,
    /// The line exactly as printed.
    pub line: String,
    /// The ledger turn the point's report counted from.
    pub turn: Option<u64>,
    /// The inbox row id of the report folded into an `EVENT turn` line.
    pub report: Option<u64>,
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
            | "RESET" => {
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

    /// One JSON object, no newline, keys in the order the module doc lists.
    pub fn to_json(&self) -> String {
        let opt_str = |s: &Option<String>| s.as_deref().map_or("null".to_string(), json_str);
        let opt_num = |n: Option<u64>| n.map_or("null".to_string(), |n| n.to_string());
        format!(
            "{{\"t\":{},\"sid\":{},\"kind\":{},\"phase\":{},\"seq\":{},\"complete\":{},\
             \"rows\":{},\"summary\":{},\"line\":{},\"turn\":{},\"report\":{}}}",
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
            Err(e) => j.warn(&format!("cannot open it: {e}"), warn),
        }
        j
    }

    /// Append the record of `line` (see [`JournalRecord::of_line`]). A write
    /// that fails is said once, to `warn`; the next line is tried all the
    /// same.
    pub fn record(&mut self, line: &str, turn: Option<u64>, warn: &mut dyn Write) {
        if self.path.is_none() {
            return;
        }
        let rec = JournalRecord::of_line(unix_ms(), self.sid.as_deref(), line, turn);
        let mut text = rec.to_json();
        text.push('\n');
        let res = match self.file.as_mut() {
            Some(f) => f.write_all(text.as_bytes()).and_then(|()| f.flush()),
            None => return,
        };
        if let Err(e) = res {
            self.warn(&format!("cannot append to it: {e}"), warn);
        }
    }

    /// Append one line that is already a JSON object (the approval ledger's rows,
    /// [`super::approvals`]), warning once like [`Self::record`]. `false` with no journal or when
    /// the write failed, for a caller that must not act on what it could not record (the disk
    /// journal's intent rows).
    pub fn append_raw(&mut self, json: &str, warn: &mut dyn Write) -> bool {
        let res = match self.file.as_mut() {
            Some(f) => f
                .write_all(json.as_bytes())
                .and_then(|()| f.write_all(b"\n"))
                .and_then(|()| f.flush()),
            None => return false,
        };
        if let Err(e) = res {
            self.warn(&format!("cannot append to it: {e}"), warn);
            return false;
        }
        true
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

    /// Every kind of line the loops print, read into its record.
    #[test]
    fn each_printed_line_is_read_into_its_fields() {
        let rec = |line: &str| JournalRecord::of_line(7, Some("@s-1"), line, None);
        let r = rec("EVENT idle seq=106 complete=1 rows=689 ⏺ Done.");
        assert_eq!(
            (
                r.kind.as_str(),
                r.phase.as_str(),
                r.seq,
                r.complete,
                r.rows,
                r.summary.as_str()
            ),
            ("event", "idle", Some(106), Some(true), Some(689), "⏺ Done.")
        );
        assert_eq!(r.sid.as_deref(), Some("s-1"));
        let r = rec("EVENT question seq=104 ⏺ Keep the harness or rewrite it?");
        assert_eq!(
            (
                r.phase.as_str(),
                r.seq,
                r.complete,
                r.rows,
                r.summary.as_str()
            ),
            (
                "question",
                Some(104),
                None,
                None,
                "⏺ Keep the harness or rewrite it?"
            )
        );
        let r = rec("EVENT prompt seq=101 kind=bash classify=not-read-only:rm command=rm -rf x");
        assert_eq!(
            (r.phase.as_str(), r.summary.as_str()),
            (
                "prompt",
                "kind=bash classify=not-read-only:rm command=rm -rf x"
            )
        );
        let r = rec("EVENT context seq=101 9% until auto-compact");
        assert_eq!(
            (r.phase.as_str(), r.summary.as_str()),
            ("context", "9% until auto-compact")
        );
        let r = rec("EVENT compacted seq=102");
        assert_eq!(
            (r.phase.as_str(), r.seq, r.summary.as_str()),
            ("compacted", Some(102), "")
        );
        let r = rec(
            "EVENT survey seq=9 dismiss with: aterm ctl @s-1 key 'if=^●.How.is.Claude.doing' 0",
        );
        assert_eq!(r.phase, "survey");
        assert!(r.summary.starts_with("dismiss with: aterm ctl @s-1"));
        let r = rec("APPROVED seq=102 git log --oneline -5");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            ("approved", "prompt", Some(102), "git log --oneline -5")
        );
        let r = rec("DISMISSED survey seq=31");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq),
            ("dismissed", "survey", Some(31))
        );
        let r = rec("RECONNECT await seq 7 failed: server closed the connection");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "reconnect",
                "-",
                None,
                "await seq 7 failed: server closed the connection"
            )
        );
        let r = rec("RECONNECTED after 1834 ms");
        assert_eq!(
            (r.kind.as_str(), r.summary.as_str()),
            ("reconnect", "after 1834 ms")
        );
        let r = rec("TIMEOUT");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.summary.as_str()),
            ("timeout", "-", "")
        );
        let r = rec("EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)");
        assert_eq!(
            (r.kind.as_str(), r.summary.as_str()),
            (
                "exit",
                "session gone (await seq 106 failed: aterm-ctl: ERR exited)"
            )
        );
        // `watch --mail`'s lines: a delivery, a turn with its report folded in, an idle with none.
        let r = rec("MAIL id=5 off=91 from=s-1@n-1 kind=report len=2048 re=88");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "mail",
                "-",
                None,
                "id=5 off=91 from=s-1@n-1 kind=report len=2048 re=88"
            )
        );
        let r = rec("EVENT turn seq=106 report=5 rows=12 ⏺ Done.");
        assert_eq!(
            (
                r.kind.as_str(),
                r.phase.as_str(),
                r.seq,
                r.report,
                r.rows,
                r.complete,
                r.summary.as_str()
            ),
            (
                "event",
                "turn",
                Some(106),
                Some(5),
                Some(12),
                None,
                "⏺ Done."
            )
        );
        let r = rec("EVENT idle-no-report seq=106 complete=1 rows=689 ⏺ Done.");
        assert_eq!(
            (r.phase.as_str(), r.report, r.complete, r.rows),
            ("idle-no-report", None, Some(true), Some(689))
        );
        // The limit episode's lines: printed and journal-only.
        let r = rec("EXTEND until=2026-09-19T18:10:00Z reset=Sep 19 at 11am (America/Los_Angeles)");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "extend",
                "-",
                None,
                "until=2026-09-19T18:10:00Z reset=Sep 19 at 11am (America/Los_Angeles)"
            )
        );
        let r = rec("ESCALATED seq=102 attention=OK mail=OK 7 off=91");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            ("escalated", "-", Some(102), "attention=OK mail=OK 7 off=91")
        );
        let r = rec("CLEARED seq=104 attention=OK resumed");
        assert_eq!(
            (r.kind.as_str(), r.seq, r.summary.as_str()),
            ("cleared", Some(104), "attention=OK resumed")
        );
        // The turn-end policy's lines: the rule id is the phase.
        let r = rec("CONTINUED seq=102 rule=usage-resume@v1 keep going");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            ("continued", "usage-resume@v1", Some(102), "keep going")
        );
        let r = rec("UNPROVEN seq=12 rule=allow-once@v1 unproven: not read-only: touch");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "unproven",
                "allow-once@v1",
                Some(12),
                "unproven: not read-only: touch"
            )
        );
        let r =
            rec("CHOSE seq=40 rule=answer-recommended@v1 policy=recommended Which colour? → Blue");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "chose",
                "answer-recommended@v1",
                Some(40),
                "policy=recommended Which colour? → Blue"
            )
        );
        // The accounts probe's line. Controls: the legacy `PROBE …` is still `probe`; an unknown
        // word is `other`, its line whole.
        let r = rec("PROBED seq=102 rule=usage-probe@v1 /usage session=1% week=10%");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "probed",
                "usage-probe@v1",
                Some(102),
                "/usage session=1% week=10%"
            )
        );
        let r = rec("PROBED seq=102 rule=usage-probe@v1 /usage no sample (fold)");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.summary.as_str()),
            ("probed", "usage-probe@v1", "/usage no sample (fold)")
        );
        // A limit reset's lines, and a reset passed over as a `skipped`. Control: the lowercase
        // word is `other`.
        for (line, summary) in [
            (
                "RESET seq=41 rule=limit-reset@v1 confirmed (Refills your weekly limit now)",
                "confirmed (Refills your weekly limit now)",
            ),
            (
                "RESET seq=41 rule=limit-reset@v1 used (1 left)",
                "used (1 left)",
            ),
            (
                "RESET seq=41 rule=limit-reset@v1 kept (inexact-early)",
                "kept (inexact-early)",
            ),
            ("RESET seq=41 rule=limit-reset@v1 unread", "unread"),
        ] {
            let r = rec(line);
            assert_eq!(
                (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
                ("reset", "limit-reset@v1", Some(41), summary)
            );
        }
        let r = rec("SKIPPED seq=41 rule=limit-reset@v1 host=attempted");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.summary.as_str()),
            ("skipped", "limit-reset@v1", "host=attempted")
        );
        assert_eq!(rec("reset seq=41 rule=limit-reset@v1 unread").kind, "other");
        // A due probe not run: the gate that shut it (and the host's word) is the summary.
        let r = rec("SKIPPED seq=102 rule=usage-probe@v1 gate=not-live");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            ("skipped", "usage-probe@v1", Some(102), "gate=not-live")
        );
        let r = rec("SKIPPED seq=102 rule=usage-probe@v1 gate=claim host=not-early");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.summary.as_str()),
            ("skipped", "usage-probe@v1", "gate=claim host=not-early")
        );
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
        // Control: `PROBE skipped gate=…` would read as the legacy limit probe, which is why the
        // line is spelled `SKIPPED`.
        let r = rec("PROBE skipped gate=not-live");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq),
            ("probe", "skipped", None)
        );
        let r = rec("PROBE sent seq=9 keep going");
        assert_eq!((r.kind.as_str(), r.phase.as_str()), ("probe", "sent"));
        let r = rec("PROBING seq=102 rule=usage-probe@v1 /usage");
        assert_eq!(
            (r.kind.as_str(), r.summary.as_str()),
            ("other", "PROBING seq=102 rule=usage-probe@v1 /usage")
        );
        let r = rec("TYPED seq=7 rule=context-compact@v1 /compact");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            ("typed", "context-compact@v1", Some(7), "/compact")
        );
        let r = rec("RESTARTED seq=8 rule=model-fallback@v1 relaunch --model opus");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "restarted",
                "model-fallback@v1",
                Some(8),
                "relaunch --model opus"
            )
        );
        let r = rec("DECLINED seq=105 rule=a-rule@v1 a box => aterm harness (not the user): …");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "declined",
                "a-rule@v1",
                Some(105),
                "a box => aterm harness (not the user): …"
            )
        );
        let r = rec("WAITING seq=9 until=2026-09-19T18:10:00Z overloaded retry 1 of 3");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "waiting",
                "-",
                Some(9),
                "until=2026-09-19T18:10:00Z overloaded retry 1 of 3"
            )
        );
        // A question decision's rows are the summary.
        let r = rec("DIALOG seq=31 rule=answer-recommended@v1 ──── ⏎  ☐ Colors ⏎ ❯ 1. Dark");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            (
                "dialog",
                "answer-recommended@v1",
                Some(31),
                "──── ⏎  ☐ Colors ⏎ ❯ 1. Dark"
            )
        );
        let r = rec("SKIPPED seq=9 rule=continue@v1 not submitted: skipped");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.summary.as_str()),
            ("skipped", "continue@v1", "not submitted: skipped")
        );
        // The legacy `PROBE` lines still read.
        let r = rec("PROBE sent seq=102");
        assert_eq!(
            (r.kind.as_str(), r.phase.as_str(), r.seq, r.summary.as_str()),
            ("probe", "sent", Some(102), "")
        );
        let r = rec("PROBE deferred seq=102 text is typed in the composer");
        assert_eq!(
            (r.phase.as_str(), r.seq, r.summary.as_str()),
            ("deferred", Some(102), "text is typed in the composer")
        );
        for (line, phase) in [
            ("EVENT resumed seq=103 ⏺ Yes: the A/B is done.", "resumed"),
            (
                "EVENT still-limited seq=103 no answer within 120 s",
                "still-limited",
            ),
            ("EVENT rebriefed seq=104", "rebriefed"),
        ] {
            let r = rec(line);
            assert_eq!(
                (r.kind.as_str(), r.phase.as_str()),
                ("event", phase),
                "{line}"
            );
            assert_eq!(r.seq, Some(if phase == "rebriefed" { 104 } else { 103 }));
        }
    }

    /// An `EXCEPTION` line is its own kind, and its end a `cleared`.
    #[test]
    fn an_exception_line_is_its_own_kind() {
        let rec = |line: &str| JournalRecord::of_line(1_789_407_079_123, Some("@s-1"), line, None);
        let r = rec(
            "EXCEPTION seq=88 kind=spend when=later Its usage credits are used up, and no time \
             window ends that. => Add usage credits in claude.ai settings.",
        );
        assert_eq!((r.kind.as_str(), r.seq), ("exception", Some(88)));
        assert!(
            r.summary
                .starts_with("kind=spend when=later Its usage credits"),
            "{}",
            r.summary
        );
        assert!(
            r.summary
                .ends_with("=> Add usage credits in claude.ai settings.")
        );
        let r = rec("CLEARED seq=91 exception=spend after=1422s the agent working");
        assert_eq!((r.kind.as_str(), r.seq), ("cleared", Some(91)));
        assert_eq!(r.summary, "exception=spend after=1422s the agent working");
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
        assert_eq!((old.report, old.turn), (None, None));
        assert!(JournalRecord::from_json("{\"kind\":\"event\"}").is_err());
        assert!(JournalRecord::from_json("{\"t\":1,\"kind\":\"event\",\"line\":").is_err());
    }
}
