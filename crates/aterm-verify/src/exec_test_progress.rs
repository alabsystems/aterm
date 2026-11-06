// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A running test binary's cases, read from its log as each one ends.
//!
//! A binary run with libtest's JSON format (`crate::libtest_json`) writes each case's `failed`
//! event, with its whole captured block, as the case ends. That block decides what the case
//! makes of its binary ([`crate::libtest::marks`]): a refusal of its machine or a full disk is an
//! environmental NOT RUN, anything else a FAIL. Once the failed cases seen are decisive
//! ([`crate::libtest::decisive`]: no reading of the cases still to come can turn the binary NOT
//! RUN), a decisive failed case ends its binary when its run halts at its first red: `exec`
//! stops it there ([`Seen::Red`]), and the binary's log already holds the block its red row's
//! "why" comes from. A spared case ([`Seen::Spared`]) halts nothing, and reading goes on.
//!
//! An event counts only as libtest writes it: a whole line, for a case that started earlier in
//! the same stream, and that case's first end. That guards against accidents (a line cut short, a
//! test's own output that looks like an event), not forgery. A line over [`LINE_CAP`] and a human
//! format's `test … FAILED` line are [`Seen::Undecided`]: that case is decided at its binary's end.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use crate::libtest::Marks;
use crate::libtest_json::{Kind, case_block, line_event};

/// The longest line read as an event; a longer one is [`Seen::Undecided`].
pub(super) const LINE_CAP: usize = 4 << 20;

/// What one failed case, read as it ended, makes of its binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// With the failed cases before it, this one makes its binary a FAIL whatever comes after;
    /// `marks`, its own block's.
    Red { case: String, marks: Marks },
    /// Its block refused its machine or hit a full disk ([`Marks`]); its binary may yet be NOT
    /// RUN.
    Spared { case: String, marks: Marks },
    /// Its block could not be read while the binary runs (a line over [`LINE_CAP`], one that does
    /// not parse, a human-format log); its binary's end decides it. `None` where the line was
    /// too long to name it.
    Undecided { case: Option<String> },
}

/// The cases of one binary's log, read as it grows.
pub(super) struct LiveCases {
    file: std::fs::File,
    json: bool,
    line: Vec<u8>,
    oversized: bool,
    started: BTreeSet<String>,
    ended: BTreeSet<String>,
    seen: Vec<Marks>,
    red: bool,
}

impl LiveCases {
    /// Read `path` from its start; `json` when the binary writes libtest's JSON events.
    pub(super) fn open(path: &Path, json: bool) -> std::io::Result<Self> {
        Ok(Self {
            file: crate::open_regular(path)?,
            json,
            line: Vec::new(),
            oversized: false,
            started: BTreeSet::new(),
            ended: BTreeSet::new(),
            seen: Vec::new(),
            red: false,
        })
    }

    /// A bounded read of newly appended output: each failed case it ends, in log order. Nothing
    /// once a case was [`Seen::Red`].
    pub(super) fn poll(&mut self) -> Vec<Seen> {
        let mut out = Vec::new();
        if self.red {
            return out;
        }
        let mut bytes = vec![0; 1 << 16];
        for _ in 0..512 {
            let Ok(n) = self.file.read(&mut bytes) else {
                return out;
            };
            if n == 0 {
                return out;
            }
            for &byte in &bytes[..n] {
                if byte != b'\n' {
                    if self.line.len() < LINE_CAP && !self.oversized {
                        self.line.push(byte);
                    } else {
                        self.oversized = true;
                    }
                    continue;
                }
                let line = std::mem::take(&mut self.line);
                let oversized = std::mem::replace(&mut self.oversized, false);
                if let Some(seen) = self.line_ended(&line, oversized) {
                    let red = matches!(seen, Seen::Red { .. });
                    out.push(seen);
                    if red {
                        return out;
                    }
                }
            }
        }
        out
    }

    fn line_ended(&mut self, line: &[u8], oversized: bool) -> Option<Seen> {
        if oversized {
            // Only a failed event is long: its block is in it.
            let head = String::from_utf8_lossy(&line[..line.len().min(4096)]);
            return (head.contains("\"failed\"") && head.starts_with('{'))
                .then_some(Seen::Undecided { case: None });
        }
        let text = String::from_utf8_lossy(line);
        if !self.json {
            return human_failed_case(&text).map(|case| Seen::Undecided { case: Some(case) });
        }
        let Some((_, ev)) = line_event(&text) else {
            let plain = text.trim_start();
            return (plain.starts_with('{') && plain.contains("\"event\": \"failed\""))
                .then_some(Seen::Undecided { case: None });
        };
        if ev.kind == Kind::Suite {
            return None;
        }
        let name = ev.name.clone()?;
        match ev.event.as_str() {
            "started" => {
                self.started.insert(name);
                None
            }
            "ok" | "failed" | "ignored" => {
                if !self.started.contains(&name) || !self.ended.insert(name.clone()) {
                    return None;
                }
                if ev.event != "failed" {
                    return None;
                }
                let marks = crate::libtest::marks(&case_block(&ev));
                self.seen.push(marks);
                if crate::libtest::decisive(&self.seen) {
                    self.red = true;
                    Some(Seen::Red { case: name, marks })
                } else {
                    Some(Seen::Spared { case: name, marks })
                }
            }
            _ => None,
        }
    }
}

/// The case a human-format `test <case> ... FAILED` line names.
fn human_failed_case(line: &str) -> Option<String> {
    let plain = if line.contains('\x1b') {
        std::borrow::Cow::Owned(crate::libtest::strip_ansi(line))
    } else {
        std::borrow::Cow::Borrowed(line)
    };
    plain
        .trim_end_matches('\r')
        .strip_prefix("test ")?
        .strip_suffix(" ... FAILED")
        .filter(|case| !case.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "exec_test_progress_tests.rs"]
mod tests;
