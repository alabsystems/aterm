// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TEST BINARIES' HISTORY (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): what
//! each test binary took the last times it ran here, kept for planning (the width and order a
//! run gives its binaries), never for a verdict. A row is a binary by its re-run spec (`-p
//! <package> <target flag>` and the run's features: [`crate::testrun::Binary::spec`]), which
//! names the same binary in every checkout and every build, where its executable's hash does not.
//! It keeps the last [`SAMPLES_KEPT`] samples of each, newest last, and at most [`ROWS_KEPT`]
//! rows: past that, the row whose newest sample is oldest goes, never the one just added.
//!
//! The file sits beside the stages' memory records in this user's aterm state directory
//! ([`aterm_capacity::book::TEST_HISTORY`]) and is written as they are: under its lock from the
//! read to the write, staged and renamed whole ([`aterm_capacity::book::update_text`]). A run
//! adds the sample of every binary whose run measured it in one write once its binaries have run
//! ([`keep`]): none for a binary its ceiling left no time to start, its environment ended, or an
//! interrupt cut short, whose wall would say nothing of it. A binary stopped at its first
//! decisive red adds an outcome-only sample: its zero wall changes no timing but makes its latest
//! FAIL available to failed-first order. The width planner reads the history ([`History::seen`])
//! before a run plans its binaries. A file that
//! cannot be decoded (another schema, a torn row) reads as empty and is replaced; one that cannot
//! be read at all (a symlink, a directory, past its bound) is left as it is.

use std::io;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

/// Select only the planning record location; CPU and memory admission keep their account book.
pub(crate) fn planning_path(
    explicit: Option<PathBuf>,
    account: Option<PathBuf>,
) -> Option<PathBuf> {
    explicit.or_else(|| account.map(|root| root.join(aterm_capacity::book::TEST_HISTORY)))
}

/// The history's schema.
pub const SCHEMA: u64 = 1;

/// The samples a row keeps, newest last.
pub const SAMPLES_KEPT: usize = 8;

/// The rows the file keeps.
pub const ROWS_KEPT: usize = 4096;

/// The encoded history's read and write bound.
const MAX_BYTES: u64 = 16 << 20;

/// What one run of a test binary took ([`crate::testrun`]'s `test-binary-finished`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    /// When it finished, unix ms.
    pub at_ms: u64,
    /// `ok`, `FAIL` or `NOT RUN`, as its event says.
    pub outcome: String,
    /// Its own wall, ms.
    pub wall_ms: u64,
    /// The CPU its tree was read using, ms ([`crate::exec::TreeUsage::cpu`]); `None` where unread.
    pub cpu_ms: Option<u64>,
    /// The part of its work it waited for a core, ms; `None` where unread.
    pub starved_ms: Option<u64>,
    /// The most its tree held at one read, bytes; `None` where unread.
    pub peak_bytes: Option<u64>,
    /// The test threads it ran at (`RUST_TEST_THREADS`).
    pub width: u32,
    /// The tests libtest said it runs ([`crate::libtest::tests_run`]); `None` where it said none.
    pub tests: Option<u64>,
    /// A case or module filter balancing observed time, where a successful long run measured one.
    pub partition: Option<String>,
}

/// One binary's samples, newest last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Its re-run spec.
    pub test: String,
    pub samples: Vec<Sample>,
}

impl Row {
    /// When its newest sample was taken.
    fn newest(&self) -> u64 {
        self.samples.iter().map(|s| s.at_ms).max().unwrap_or(0)
    }
}

impl Sample {
    /// The test threads it had: its width, no more than its tests, at least one.
    fn threads(&self) -> u64 {
        u64::from(self.width).min(self.tests.unwrap_or(1)).max(1)
    }

    /// Its CPU per thread it had (`cpu / wall / threads`), where both were read.
    fn per_thread(&self) -> Option<f64> {
        let cpu = self.cpu_ms?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "milliseconds and thread counts far below 2^52"
        )]
        let per = cpu as f64 / self.wall_ms as f64 / self.threads() as f64;
        (self.wall_ms > 0 && per.is_finite()).then_some(per)
    }

    /// Whether its wall says what the binary takes: a run outside the tree's control (NOT RUN),
    /// one with no wall, and a FAIL in which libtest never said it runs a test (it crashed before
    /// it began) do not. A FAIL that ran its tests, or one the run's ceiling ended, is a lower
    /// bound of what it takes, which plans it no shorter than it is.
    fn timed(&self) -> bool {
        use crate::ladder::{Outcome, Severity};
        self.outcome != crate::report::word(Outcome::Fail(Severity::CouldNotRun))
            && self.wall_ms > 0
            && (self.outcome == crate::report::word(Outcome::Ok) || self.tests.is_some())
    }

    /// Whether it was a FAIL: a proven red of the binary's own.
    fn failed(&self) -> bool {
        use crate::ladder::{Outcome, Severity};
        self.outcome == crate::report::word(Outcome::Fail(Severity::GateFailed))
    }
}

/// The history the file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    rows: Vec<Row>,
}

impl History {
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row of the binary `test`, if one is kept.
    #[must_use]
    pub fn row(&self, test: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.test == test)
    }

    /// Add `sample` to `test`'s row, a new row if it has none: its oldest sample goes once it has
    /// more than [`SAMPLES_KEPT`], and the history's row whose newest sample is oldest once it has
    /// more than [`ROWS_KEPT`], never `test`'s.
    pub fn push(&mut self, test: &str, sample: Sample) {
        let mut at = match self.rows.iter().position(|r| r.test == test) {
            Some(at) => at,
            None => {
                self.rows.push(Row {
                    test: test.to_owned(),
                    samples: Vec::new(),
                });
                self.rows.len() - 1
            }
        };
        let samples = &mut self.rows[at].samples;
        samples.push(sample);
        let over = samples.len().saturating_sub(SAMPLES_KEPT);
        samples.drain(..over);
        while self.rows.len() > ROWS_KEPT {
            let Some(oldest) = (0..self.rows.len())
                .filter(|i| *i != at)
                .min_by_key(|i| self.rows[*i].newest())
            else {
                break;
            };
            self.rows.remove(oldest);
            if oldest < at {
                at -= 1;
            }
        }
    }

    /// What the width planner reads of the binary `test` ([`crate::test_plan`]): its last timed
    /// sample's wall, width and tests, the most CPU per thread and the most memory
    /// any kept sample showed, and whether its last sample was a FAIL. `None` with no timed
    /// sample.
    #[must_use]
    pub fn seen(&self, test: &str) -> Option<crate::test_plan::Seen> {
        let row = self.row(test)?;
        let last = row.samples.iter().rev().find(|s| s.timed())?;
        let per_thread = row
            .samples
            .iter()
            .filter(|s| s.timed())
            .filter_map(Sample::per_thread)
            .reduce(f64::max)
            .unwrap_or(1.0);
        Some(crate::test_plan::Seen {
            wall: std::time::Duration::from_millis(last.wall_ms),
            width: last.width.max(1),
            tests: u32::try_from(last.tests.unwrap_or(0)).unwrap_or(u32::MAX),
            per_thread,
            peak: row
                .samples
                .iter()
                .filter_map(|s| s.peak_bytes)
                .max()
                .unwrap_or(0),
            failed: row.samples.last().is_some_and(Sample::failed),
        })
    }

    /// The most memory any kept run of a binary of `crates` held at once (every binary's where
    /// `crates` is empty: the whole workspace), bytes; 0 with none read.
    #[must_use]
    pub fn heaviest(&self, crates: &[String]) -> u64 {
        self.rows
            .iter()
            .filter(|r| {
                crates.is_empty()
                    || r.test
                        .strip_prefix("-p ")
                        .and_then(|rest| rest.split(' ').next())
                        .is_some_and(|c| crates.iter().any(|k| k == c))
            })
            .flat_map(|r| &r.samples)
            .filter_map(|s| s.peak_bytes)
            .max()
            .unwrap_or(0)
    }

    /// The history as its file holds it.
    #[must_use]
    pub fn encode(&self) -> String {
        let opt = |v: Option<u64>| v.map_or(Value::Null, Value::from);
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|r| {
                let samples = r
                    .samples
                    .iter()
                    .map(|s| {
                        let mut o = Map::new();
                        o.insert("at_ms".to_owned(), Value::from(s.at_ms));
                        o.insert("outcome".to_owned(), Value::from(s.outcome.as_str()));
                        o.insert("wall_ms".to_owned(), Value::from(s.wall_ms));
                        o.insert("cpu_ms".to_owned(), opt(s.cpu_ms));
                        o.insert("starved_ms".to_owned(), opt(s.starved_ms));
                        o.insert("peak_bytes".to_owned(), opt(s.peak_bytes));
                        o.insert("width".to_owned(), Value::from(s.width));
                        o.insert("tests".to_owned(), opt(s.tests));
                        o.insert(
                            "partition".to_owned(),
                            s.partition.as_deref().map_or(Value::Null, Value::from),
                        );
                        Value::Object(o)
                    })
                    .collect();
                let mut o = Map::new();
                o.insert("test".to_owned(), Value::from(r.test.as_str()));
                o.insert("samples".to_owned(), Value::Array(samples));
                Value::Object(o)
            })
            .collect();
        let mut file = Map::new();
        file.insert("schema".to_owned(), Value::from(SCHEMA));
        file.insert("rows".to_owned(), Value::Array(rows));
        aterm_json::to_string(&Value::Object(file)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The history `text` holds; empty for text that is not [`Self::encode`]'s, of another
    /// schema, or with any row or sample it cannot read whole.
    #[must_use]
    pub fn decode(text: &str) -> History {
        let decoded = || -> Option<History> {
            let file: Value = aterm_json::from_str(text).ok()?;
            if file.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
                return None;
            }
            let opt = |s: &Value, k: &str| -> Option<Option<u64>> {
                match s.get(k)? {
                    Value::Null => Some(None),
                    v => v.as_u64().map(Some),
                }
            };
            let rows = file
                .get("rows")?
                .as_array()?
                .iter()
                .map(|r| {
                    let samples = r
                        .get("samples")?
                        .as_array()?
                        .iter()
                        .map(|s| {
                            Some(Sample {
                                at_ms: s.get("at_ms")?.as_u64()?,
                                outcome: s.get("outcome")?.as_str()?.to_owned(),
                                wall_ms: s.get("wall_ms")?.as_u64()?,
                                cpu_ms: opt(s, "cpu_ms")?,
                                starved_ms: opt(s, "starved_ms")?,
                                peak_bytes: opt(s, "peak_bytes")?,
                                width: u32::try_from(s.get("width")?.as_u64()?).ok()?,
                                tests: opt(s, "tests")?,
                                partition: match s.get("partition") {
                                    None | Some(Value::Null) => None,
                                    Some(value) => Some(value.as_str()?.to_owned()),
                                },
                            })
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(Row {
                        test: r.get("test")?.as_str()?.to_owned(),
                        samples,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(History { rows })
        };
        decoded().unwrap_or_default()
    }
}

/// The history kept at `path`; empty where it is absent or cannot be read or decoded. For
/// planning: a run reads it before it orders its binaries.
#[must_use]
pub fn load(path: &Path) -> History {
    aterm_capacity::book::read_text(path, MAX_BYTES)
        .ok()
        .flatten()
        .map(|text| History::decode(&text))
        .unwrap_or_default()
}

/// Add each binary's sample, `(spec, sample)` in the order they finished, to the history at
/// `path` in one write, under its lock from the read to the write. Nothing is read or written
/// for no sample.
///
/// # Errors
/// As [`aterm_capacity::book::update_text`]: a file that exists and cannot be read is left as it
/// is, and one too large to write leaves the history before it standing.
pub fn keep(path: &Path, samples: Vec<(String, Sample)>) -> io::Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    aterm_capacity::book::update_text(path, MAX_BYTES, ".test-binaries-", |text| {
        let mut history = text.map(History::decode).unwrap_or_default();
        for (test, sample) in samples {
            history.push(&test, sample);
        }
        Some(history.encode())
    })
    .map(|_| ())
}

#[cfg(test)]
#[path = "test_history_tests.rs"]
mod tests;
