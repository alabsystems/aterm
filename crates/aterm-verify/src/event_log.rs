// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A run's own file of its events, apart from its output (`--events`): what a release's cutter
//! keeps its records from (A19) and `verify land` judges by (B10).
//!
//! The `check-finished` and `test-binary-finished` lines it keeps go to the run's output too, which its children share: there a child's unfinished line,
//! or a line longer than a pipe writes at once, can land inside one. Here each line is one
//! `write_all` under one lock, to a file opened close-on-exec, so no child inherits it and no two
//! lines interleave.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::Mutex;

/// The events file a run writes (`--events`).
#[derive(Debug)]
pub struct EventLog {
    file: Mutex<File>,
}

impl EventLog {
    /// Open `path` to append to (made, owner-only, when it is not there), close-on-exec as every
    /// file std opens is.
    ///
    /// # Errors
    /// It cannot be opened.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut open = std::fs::OpenOptions::new();
        open.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        Ok(Self {
            file: Mutex::new(open.open(path)?),
        })
    }

    /// Append `line` (a `verify-event: ` line; its newline added when it has none) whole. A
    /// write that fails loses the line, which its reader takes as nothing said: a release's
    /// cutter runs a check with no record again, and `verify land`, whose run's last line
    /// (`run-finished`) then names a check no line holds, or is lost itself, pushes nothing.
    pub fn write(&self, line: &str) {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let mut whole = String::with_capacity(line.len() + 1);
        whole.push_str(line);
        whole.push('\n');
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = file.write_all(whole.as_bytes());
    }
}

/// One dev-set run: its exit code, each finished check's row, its wall seconds, and what its
/// last event says it finished.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Judged {
    /// The run's exit code ([`crate::exit`]).
    pub code: i32,
    /// Each check the run finished, in the order it finished them.
    pub checks: Vec<CheckRow>,
    /// The run's wall seconds.
    pub secs: u64,
    /// The run's own `run-finished` event ([`crate::report::run_event`]); `None` where none was read.
    pub finished: Option<Finished>,
}

impl Judged {
    /// Whether its rows are the run's whole record: its `run-finished` event names its exit code
    /// and exactly the checks its rows name, each with its word. A line the events file lost
    /// leaves a check the event names with no row, or the event itself missing.
    #[must_use]
    pub fn whole(&self) -> bool {
        let mut rows: Vec<(&str, &str)> = self
            .checks
            .iter()
            .map(|row| (row.check.as_str(), row.word.as_str()))
            .collect();
        rows.sort_unstable();
        self.finished.as_ref().is_some_and(|finished| {
            let mut named: Vec<(&str, &str)> = finished
                .checks
                .iter()
                .map(|(check, word)| (check.as_str(), word.as_str()))
                .collect();
            named.sort_unstable();
            finished.code == self.code && named == rows
        })
    }
}

/// What a run's `run-finished` event states: its exit code and each check it finished, with its
/// outcome word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// The run's exit code.
    pub code: i32,
    /// Each check and its word.
    pub checks: Vec<(String, String)>,
}

impl Finished {
    /// What a `verify-event: {…}` line states, when it is a `run-finished` event.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let value: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ")?).ok()?;
        if value.get("event")?.as_str()? != "run-finished" {
            return None;
        }
        let checks = value
            .get("checks")?
            .as_array()?
            .iter()
            .map(|c| {
                Some((
                    c.get("check")?.as_str()?.to_owned(),
                    c.get("outcome")?.as_str()?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            code: i32::try_from(value.get("code")?.as_i64()?).ok()?,
            checks,
        })
    }
}

/// One finished check, as its `check-finished` event states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRow {
    /// The check's name.
    pub check: String,
    /// Its outcome word: `ok`, `skip`, `FAIL` or `NOT RUN`.
    pub word: String,
    /// The ids of the findings the event carries (at most `report::FINDINGS_SHOWN`).
    pub findings: Vec<String>,
    /// How many findings the check had.
    pub findings_total: usize,
}

impl CheckRow {
    /// The row a `verify-event: {…}` line states, when it is a `check-finished` event.
    #[must_use]
    pub fn of_event(line: &str) -> Option<Self> {
        let json = line.strip_prefix("verify-event: ")?;
        let value: aterm_json::Value = aterm_json::from_str(json).ok()?;
        if value.get("event")?.as_str()? != "check-finished" {
            return None;
        }
        let findings = value
            .get("findings")?
            .as_array()?
            .iter()
            .map(|f| f.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            check: value.get("check")?.as_str()?.to_owned(),
            word: value.get("outcome")?.as_str()?.to_owned(),
            findings,
            findings_total: usize::try_from(value.get("findings_total")?.as_u64()?).ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines from many threads, each far longer than a pipe writes at once, land whole and one
    /// per line, a missing newline added and a present one kept single.
    #[test]
    fn each_line_lands_whole_whatever_writes_beside_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let at = dir.path().join("06-verify.events");
        let log = EventLog::open(&at).unwrap();
        let long = |t: usize, i: usize| {
            format!(
                "verify-event: {{\"t\":{t},\"i\":{i},\"pad\":\"{}\"}}",
                "x".repeat(20_000)
            )
        };
        std::thread::scope(|s| {
            for t in 0..4 {
                let log = &log;
                s.spawn(move || {
                    for i in 0..20 {
                        let line = long(t, i);
                        log.write(&if i % 2 == 0 {
                            line
                        } else {
                            format!("{line}\n")
                        });
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&at).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 80);
        for line in lines {
            let json = line.strip_prefix("verify-event: ").expect("a whole line");
            let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
            assert_eq!(event["pad"].as_str().map(str::len), Some(20_000));
        }
    }
}
