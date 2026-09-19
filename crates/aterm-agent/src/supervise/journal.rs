// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `watch --journal FILE` / `supervise --journal FILE`: the loop's own record
//! of what it decided, one JSON object per line, so `aterm drive ledger` can
//! replay how the loop ran.
//!
//! Measured on 2026-09-14: the watcher's `--notes` file held none of its own
//! EVENT, APPROVED or DISMISSED decisions — only the manager's hand-written
//! lines — so nothing could say when the worker stopped, how long the manager
//! took to answer, or what was waved through.
//!
//! Every line either loop prints is journaled as it is printed — `watch`'s
//! stdout lines, `supervise`'s stderr lines, and for `supervise` the lines
//! `watch` would have printed for what it decides silently (its approvals, its
//! review point, its TIMEOUT or the error it ends on, as an `EXIT` line) — as
//!
//! ```text
//! {"t":<unix ms>,"sid":"<sid>"|null,
//!  "kind":"event|approved|dismissed|reconnect|timeout|exit|mail|extend|escalated|cleared|probe",
//!  "phase":"idle|question|prompt|limited|survey|context|compacted|turn|idle-no-report|resumed|
//!           still-limited|rebriefed|rebrief-failed|-",
//!  "seq":<n>|null,"complete":0|1|null,"rows":<n>|null,"summary":"<the line's free-text tail>",
//!  "line":"<the exact line>","turn":<id>|null,"report":<id>|null}
//! ```
//!
//! read from the line itself ([`JournalRecord::of_line`]), so the record and
//! the line cannot disagree. `turn` is the ledger turn the point's report was
//! counted from (`watch --report`), else `null`; `report` is the inbox row id
//! of the worker's report `watch --mail` folded into an `EVENT turn` line
//! (its `rows=` is that body's row count), else `null`; a `MAIL …` line is
//! `kind` `mail`, its words the summary. A limit episode's lines (round 17):
//! `EXTEND until=<UTC> reset=<text>` is `extend`; the journal-only `ESCALATED
//! seq=<n> …`, `CLEARED seq=<n> …` and `PROBE <sent|deferred> seq=<n> …` are
//! `escalated`, `cleared` and `probe`, their `seq=` read like an EVENT's; the
//! probe's outcome is an EVENT under `resumed`, `still-limited`, `rebriefed`
//! or `rebrief-failed`. The file is opened
//! append-only, created `0600` when missing; a failure to open or write it is
//! said ONCE on stderr and never stops the loop — the journal is a record of
//! the watch, not a condition of it.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::screen::Json;

/// One journaled line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    /// When it was printed, Unix milliseconds.
    pub t: i64,
    /// The session the loop watched (the `@sid` it was given, without the
    /// `@`), `None` when it was given none.
    pub sid: Option<String>,
    /// `event`, `approved`, `dismissed`, `reconnect`, `timeout`, `exit`,
    /// `mail`, `extend`, `escalated`, `cleared` or `probe` (`other` for a
    /// line none of those start).
    pub kind: String,
    /// The phase the line is about, `-` for none.
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
    /// The record of `line`, printed at `t`: its kind, phase, `seq=`,
    /// `complete=`/`rows=` (or `report=`/`rows=`) and free-text tail, read
    /// from the words the loop prints (`EVENT <phase> seq=<n> [complete=<0|1>
    /// rows=<n>] <summary>`, `EVENT turn seq=<n> report=<id> rows=<n>
    /// <summary>`, `APPROVED seq=<n> <command>`, `DISMISSED survey seq=<n>`,
    /// `RECONNECT <reason>`, `RECONNECTED after <ms> ms`, `TIMEOUT`, `EXIT
    /// <reason>`, `MAIL id=<n> …`, `EXTEND until=<UTC> …`, and the limit
    /// episode's journal-only `ESCALATED seq=<n> …`, `CLEARED seq=<n> …`,
    /// `PROBE <sent|deferred> seq=<n> …`).
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

/// `s` as a JSON string: `"` and `\` escaped, control characters as `\n`,
/// `\t`, `\r` or `\u00XX`; everything else verbatim (UTF-8).
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The current time, Unix milliseconds.
pub fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
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
            "aterm-drive: journal {path}: {why} — the loop goes on without it"
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
        // `watch --mail`'s lines: a delivery, a turn with its report folded
        // in, and an idle no report came for.
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
        // The limit episode's lines (round 17): printed and journal-only.
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
