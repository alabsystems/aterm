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
//! Libtest's long-running case event is also reported live ([`Seen::RunningLong`]), once per
//! started, unfinished case; it decides no verdict and halts nothing.
//!
//! An event counts only as libtest writes it: a whole line, for a case that started earlier in
//! the same stream, and that case's first end. That guards against accidents (a line cut short, a
//! test's own output that looks like an event), not forgery. A line over [`LINE_CAP`] and a human
//! format's `test … FAILED` line are [`Seen::Undecided`]: that case is decided at its binary's end.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::libtest::Marks;
use crate::libtest_json::{Kind, case_block, line_event};

/// The longest line read as an event; a longer one is [`Seen::Undecided`].
pub(super) const LINE_CAP: usize = 4 << 20;

/// One bounded diagnostic, classified from its full block before clipping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub case: String,
    pub output: String,
    pub omitted_bytes: usize,
    pub(crate) marks: Marks,
    pub source_path: Option<String>,
    pub start_line: Option<u64>,
    pub start_col: Option<u64>,
}

/// One completed case's libtest execution time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timing {
    pub case: String,
    pub execution: Duration,
}

/// Observations retained independently of human rendering, never a binary verdict.
#[derive(Clone, Debug, Default)]
pub struct Observations {
    pub read_end: ReadEnd,
    pub json: bool,
    pub failures: Vec<Failure>,
    pub timings: Vec<Timing>,
}

/// Why the last bounded read ended; an empty batch alone never means EOF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadEnd {
    Budget,
    Eof,
    Partial,
    #[default]
    Unread,
}

impl ReadEnd {
    pub(crate) const fn is_eof(self) -> bool {
        matches!(self, Self::Eof)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Eof => "eof",
            Self::Partial => "partial",
            Self::Unread => "unread",
        }
    }
}

fn diagnostic(case: String, block: &str, marks: Marks) -> Failure {
    let cap = crate::testrun::FAILURE_OUTPUT_BYTES;
    let cost = crate::report::json_char_bytes;
    let (output, omitted_bytes) = if block.chars().map(cost).sum::<usize>() <= cap {
        (block.to_owned(), 0)
    } else {
        let marker = "\n…\n";
        let room = cap - marker.chars().map(cost).sum::<usize>();
        let mut used = 0;
        let mut head = 0;
        for (at, c) in block.char_indices() {
            if used + cost(c) > room / 2 {
                break;
            }
            used += cost(c);
            head = at + c.len_utf8();
        }
        let mut used = 0;
        let mut tail = block.len();
        for (at, c) in block.char_indices().rev() {
            if at < head || used + cost(c) > room - room / 2 {
                break;
            }
            used += cost(c);
            tail = at;
        }
        (
            format!("{}{marker}{}", &block[..head], &block[tail..]),
            tail - head,
        )
    };
    Failure {
        case,
        output,
        omitted_bytes,
        marks,
        source_path: None,
        start_line: None,
        start_col: None,
    }
}

/// What a case reports while its binary runs: a long wait or its failed block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// Libtest reports that a still-running case exceeded its reporting interval. This is
    /// progress information, never a failure or permission to stop the binary.
    RunningLong { case: String },
    /// With the failed cases before it, this one makes its binary a FAIL whatever comes after;
    /// `marks`, its own block's; `execution`, this terminal event's libtest time when present.
    Red {
        case: String,
        marks: Marks,
        execution: Option<Duration>,
    },
    /// Its block refused its machine or hit a full disk ([`Marks`]); its binary may yet be NOT
    /// RUN.
    Spared {
        case: String,
        marks: Marks,
        execution: Option<Duration>,
    },
    /// Its block could not be read while the binary runs (a line over [`LINE_CAP`], one that does
    /// not parse, a human-format log); its binary's end decides it. `None` where the line was
    /// too long to name it.
    Undecided { case: Option<String> },
}

/// The cases of one binary's log, read as it grows.
pub(super) struct LiveCases {
    observations: Observations,
    file: std::fs::File,
    json: bool,
    line: Vec<u8>,
    oversized: bool,
    /// Complete output lines read from the binary. A partial line is no progress until its
    /// newline arrives.
    lines: u64,
    /// False after the capture file returned an error, until a later read succeeds. Wall time
    /// plus an unread output stream cannot prove that the binary printed nothing.
    readable: bool,
    started: BTreeSet<String>,
    ended: BTreeSet<String>,
    long_running: BTreeSet<String>,
    seen: Vec<Marks>,
    red: bool,
}

impl LiveCases {
    /// Read `path` from its start; `json` when the binary writes libtest's JSON events.
    pub(super) fn open(path: &Path, json: bool) -> std::io::Result<Self> {
        Ok(Self {
            observations: Observations {
                json,
                ..Default::default()
            },
            file: crate::open_regular(path)?,
            json,
            line: Vec::new(),
            oversized: false,
            lines: 0,
            readable: true,
            started: BTreeSet::new(),
            ended: BTreeSet::new(),
            long_running: BTreeSet::new(),
            seen: Vec::new(),
            red: false,
        })
    }

    /// A bounded read of newly appended output: failed cases and long-running reports, in log
    /// order. After a case is [`Seen::Red`], lines and case ends still update liveness and
    /// bounded failure diagnostics, but no later failure emits another decisive signal.
    pub(super) fn poll(&mut self) -> Vec<Seen> {
        let mut out = Vec::new();
        self.observations.read_end = ReadEnd::Budget;
        let mut bytes = vec![0; 1 << 16];
        for _ in 0..512 {
            let n = match self.file.read(&mut bytes) {
                Ok(n) => {
                    self.readable = true;
                    n
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.readable = false;
                    self.observations.read_end = ReadEnd::Unread;
                    return out;
                }
            };
            if n == 0 {
                self.observations.read_end = if self.line.is_empty() && !self.oversized {
                    ReadEnd::Eof
                } else {
                    ReadEnd::Partial
                };
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
                self.lines = self.lines.saturating_add(1);
                if let Some(seen) = self.line_ended(&line, oversized) {
                    out.push(seen);
                }
            }
        }
        out
    }

    pub(super) fn observations(&self) -> &Observations {
        &self.observations
    }

    /// Complete output lines consumed so far. A test's unterminated bytes do not move it.
    pub(super) const fn lines(&self) -> Option<u64> {
        if self.readable {
            Some(self.lines)
        } else {
            None
        }
    }

    /// Libtest JSON cases that started and have not reported `ok`, `failed` or `ignored`, sorted
    /// by name. A human-format run has no such evidence and returns none.
    pub(super) fn running(&self) -> Vec<String> {
        if !self.json {
            return Vec::new();
        }
        self.started.difference(&self.ended).cloned().collect()
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
            "timeout"
                if self.started.contains(&name)
                    && !self.ended.contains(&name)
                    && self.long_running.insert(name.clone()) =>
            {
                Some(Seen::RunningLong { case: name })
            }
            "ok" | "failed" | "ignored" => {
                if !self.started.contains(&name) || !self.ended.insert(name.clone()) {
                    return None;
                }
                let execution = if ev.kind == Kind::Test && ev.event != "ignored" {
                    ev.exec_time
                        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                } else {
                    None
                };
                if let Some(execution) = execution {
                    self.observations.timings.push(Timing {
                        case: name.clone(),
                        execution,
                    });
                }
                if ev.event != "failed"
                    || (self.red
                        && self.observations.failures.len() >= crate::report::FINDINGS_SHOWN)
                {
                    return None;
                }
                let block = case_block(&ev);
                let marks = crate::libtest::marks(&block);
                if self.observations.failures.len() < crate::report::FINDINGS_SHOWN {
                    let mut failure = diagnostic(name.clone(), &block, marks);
                    failure.source_path = ev
                        .source_path
                        .as_ref()
                        .map(|p| crate::report::clip_json(p, crate::report::EVENT_ROW_BYTES));
                    failure.start_line = ev.start_line;
                    failure.start_col = ev.start_col;
                    self.observations.failures.push(failure);
                }
                if self.red {
                    return None;
                }
                self.seen.push(marks);
                if crate::libtest::decisive(&self.seen) {
                    self.red = true;
                    Some(Seen::Red {
                        case: name,
                        marks,
                        execution,
                    })
                } else {
                    Some(Seen::Spared {
                        case: name,
                        marks,
                        execution,
                    })
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
