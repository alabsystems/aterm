// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TEST BINARIES' HISTORY (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): what
//! each test binary took the last times it ran here, kept for planning (the width and order a
//! run gives its binaries), never for a verdict. A row is a binary by its re-run spec (`-p
//! <package> <target flag>` and the run's features: [`crate::testrun::Binary::spec`]), which
//! names the same binary in every checkout and every build, where its executable's hash does not.
//! It keeps the last [`SAMPLES_KEPT`] samples of each, newest last, the grant each settled at
//! ([`Row::settled`]), which outlives those samples while its executable is unchanged, and at most [`ROWS_KEPT`] rows: past that,
//! the row whose newest sample is oldest goes, never the one just added.
//!
//! The file sits beside the stages' memory records in this user's aterm state directory
//! ([`aterm_capacity::book::TEST_HISTORY`]) and is written as they are: under its lock from the
//! read to the write, staged and renamed whole ([`aterm_capacity::book::update_text`]). A run
//! adds the sample of every binary whose run measured it in one write once its binaries have run
//! ([`keep`]): none for a binary its ceiling left no time to start, its environment ended, or an
//! interrupt cut short, whose wall would say nothing of it. A binary stopped at its first
//! decisive red adds an outcome-only sample: its zero wall changes no timing but makes its latest
//! FAIL visible without displacing longer recorded work. The width planner reads the history ([`History::seen`])
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

/// The share of its grant a run must use for the grant to have held it ([`Sample::held`]): a
/// pool of `g` threads busy the whole run uses `g` cores, less its serial start and end.
pub const HELD: f64 = 0.9;

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
    /// The cores each test thread was granted (`ATERM_TEST_CORES`, `exec::Cmd::test_width`);
    /// `None` for a run no grant bounded (one kept before grants, absent from its file).
    pub cores: Option<u32>,
    /// The tests libtest said it runs ([`crate::libtest::tests_run`]); `None` where it said none.
    pub tests: Option<u64>,
    /// A case or module filter balancing observed time, where a successful long run measured one.
    pub partition: Option<String>,
    /// The SHA-256 of the executable that ran (`test_content::Evidence::exe_sha256`); `None`
    /// where it was not read (a libtest JSON probe's run, a sample kept before it was).
    pub exe_sha256: Option<String>,
}

/// The grant a binary settled at ([`Row::settled`]) and the executable it settled as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// The largest grant a timed run of it was not held at.
    pub cores: u32,
    /// The executable that showed it ([`Sample::exe_sha256`]); `None` where none was read.
    pub exe_sha256: Option<String>,
}

/// One binary's samples, newest last, and the grant it settled at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Its re-run spec.
    pub test: String,
    pub samples: Vec<Sample>,
    /// The largest grant a timed run of it was not held at ([`Sample::held`]) when no kept run
    /// had been held at a larger one ([`Row::settle`]), with the executable that showed it;
    /// `None` until one was, and again once a timed run is held at this grant or above or a
    /// run of another executable is kept. It outlives the samples that showed it, so an
    /// unchanged binary held run after run at a smaller grant (a single-threaded test at one
    /// core) is not re-probed at twice that grant each time the unheld run ages out of the kept
    /// samples, which would also change its `ATERM_TEST_CORES` and so its content key. A
    /// changed binary's need is unknown again: one that now uses every core of the smaller
    /// grant for more work than before looks, at that grant, as it did, so only a probe above
    /// it can tell, once its kept samples no longer settle it.
    pub settled: Option<Settled>,
}

impl Row {
    /// When its newest sample was taken.
    fn newest(&self) -> u64 {
        self.samples.iter().map(|s| s.at_ms).max().unwrap_or(0)
    }

    /// Fold its newest sample, just pushed, into [`Row::settled`]. A run of an executable other
    /// than the one the binary settled as (both read) clears it: the binary changed. Then a timed
    /// run with a CPU reading and a grant `g`: held, it clears a settled grant at most `g` (the
    /// binary now uses that grant, so what it needs above it is unknown again); not held, with no
    /// kept timed run held at a grant above `g`, it makes the settled grant at least `g`, settled
    /// as its executable. Any other run changes nothing more: one no grant bounded says nothing
    /// of a grant.
    fn settle(&mut self) {
        let Some(newest) = self.samples.last() else {
            return;
        };
        let changed = |s: &Settled| {
            s.exe_sha256
                .as_deref()
                .zip(newest.exe_sha256.as_deref())
                .is_some_and(|(was, now)| was != now)
        };
        if self.settled.as_ref().is_some_and(changed) {
            self.settled = None;
        }
        if !newest.timed() {
            return;
        }
        let (Some(per_thread), Some(g)) = (newest.per_thread(), newest.cores) else {
            return;
        };
        if newest.held(per_thread) {
            if self.settled.as_ref().is_some_and(|k| g >= k.cores) {
                self.settled = None;
            }
            return;
        }
        let held = self
            .samples
            .iter()
            .filter(|s| s.timed())
            .filter(|s| s.per_thread().is_some_and(|p| s.held(p)))
            .filter_map(|s| s.cores)
            .max();
        if held.is_none_or(|h| g >= h) {
            let was = self.settled.take();
            self.settled = Some(Settled {
                cores: was.as_ref().map_or(g, |k| k.cores.max(g)),
                exe_sha256: newest.exe_sha256.clone().or(was.and_then(|k| k.exe_sha256)),
            });
        }
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

    /// Whether its grant held it: it used at least [`HELD`] of the cores each test thread was
    /// granted, so what it would use with more is unknown, not what it used.
    fn held(&self, per_thread: f64) -> bool {
        self.cores
            .is_some_and(|cores| per_thread >= HELD * f64::from(cores.max(1)))
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
                    settled: None,
                });
                self.rows.len() - 1
            }
        };
        let row = &mut self.rows[at];
        row.samples.push(sample);
        let over = row.samples.len().saturating_sub(SAMPLES_KEPT);
        row.samples.drain(..over);
        row.settle();
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
    /// sample's wall, width and tests, the CPU per thread it needs ([`demand`]), the most memory
    /// any kept sample showed, and whether its last sample was a FAIL. `None` with no timed
    /// sample.
    #[must_use]
    pub fn seen(&self, test: &str) -> Option<crate::test_plan::Seen> {
        let row = self.row(test)?;
        let last = row.samples.iter().rev().find(|s| s.timed())?;
        let per_thread = demand(
            row.samples.iter().filter(|s| s.timed()),
            row.settled.as_ref().map(|s| s.cores),
        )
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
                        o.insert("cores".to_owned(), s.cores.map_or(Value::Null, Value::from));
                        o.insert("tests".to_owned(), opt(s.tests));
                        o.insert(
                            "partition".to_owned(),
                            s.partition.as_deref().map_or(Value::Null, Value::from),
                        );
                        o.insert(
                            "exe_sha256".to_owned(),
                            s.exe_sha256.as_deref().map_or(Value::Null, Value::from),
                        );
                        Value::Object(o)
                    })
                    .collect();
                let mut o = Map::new();
                o.insert("test".to_owned(), Value::from(r.test.as_str()));
                o.insert("samples".to_owned(), Value::Array(samples));
                o.insert(
                    "settled".to_owned(),
                    r.settled.as_ref().map_or(Value::Null, |s| {
                        let mut o = Map::new();
                        o.insert("cores".to_owned(), Value::from(s.cores));
                        o.insert(
                            "exe_sha256".to_owned(),
                            s.exe_sha256.as_deref().map_or(Value::Null, Value::from),
                        );
                        Value::Object(o)
                    }),
                );
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
                                cores: match s.get("cores") {
                                    None | Some(Value::Null) => None,
                                    Some(value) => Some(u32::try_from(value.as_u64()?).ok()?),
                                },
                                tests: opt(s, "tests")?,
                                partition: match s.get("partition") {
                                    None | Some(Value::Null) => None,
                                    Some(value) => Some(value.as_str()?.to_owned()),
                                },
                                exe_sha256: text_or_null(s, "exe_sha256")?,
                            })
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(Row {
                        test: r.get("test")?.as_str()?.to_owned(),
                        samples,
                        settled: match r.get("settled") {
                            None | Some(Value::Null) => None,
                            Some(value) => Some(Settled {
                                cores: u32::try_from(value.get("cores")?.as_u64()?).ok()?,
                                exe_sha256: text_or_null(value, "exe_sha256")?,
                            }),
                        },
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(History { rows })
        };
        decoded().unwrap_or_default()
    }
}

/// The text at `key` of `o`: `Some(None)` where it is absent or null, `None` where it is not text.
fn text_or_null(o: &Value, key: &str) -> Option<Option<String>> {
    match o.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(value) => Some(Some(value.as_str()?.to_owned())),
    }
}

/// The CPU per test thread a binary's timed `samples` and its row's `settled` grant
/// ([`Row::settled`]) say it needs; `None` with no CPU reading.
///
/// A run is granted its charge, so it uses no more than its grant, and its charge comes from what
/// it used: read alone, a binary's grant could only fall. So a run its grant held
/// ([`Sample::held`]) says only that it needs at least that grant. Where no kept run with a grant
/// at least the largest that held one (or with none at all) went unheld, and the row settled at
/// no grant that large, the binary is read as needing twice that grant, and its next charge grows
/// until a run is not held or the share clamps it. Otherwise the most CPU per thread any run
/// showed is what it needs: a single-threaded test held at one core, then not held at two,
/// settles at what it used, and stays settled after the run at two ages out of the kept samples.
fn demand<'a>(samples: impl Iterator<Item = &'a Sample>, settled: Option<u32>) -> Option<f64> {
    let read: Vec<(&Sample, f64)> = samples.filter_map(|s| Some((s, s.per_thread()?))).collect();
    let used = read.iter().map(|&(_, p)| p).reduce(f64::max)?;
    let held = read
        .iter()
        .filter(|&&(s, p)| s.held(p))
        .filter_map(|(s, _)| s.cores)
        .max();
    let Some(held) = held else {
        return Some(used);
    };
    let settled = settled.is_some_and(|k| k >= held)
        || read
            .iter()
            .any(|&(s, p)| !s.held(p) && s.cores.is_none_or(|cores| cores >= held));
    Some(if settled {
        used
    } else {
        used.max(2.0 * f64::from(held.max(1)))
    })
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
