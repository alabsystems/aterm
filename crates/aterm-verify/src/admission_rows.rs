// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RUN'S ROWS IN THE ADMISSION LOG ([`aterm_capacity::admission_log`],
//! `docs/DESIGN-landing-economy.md` slice 2, sub-slice 2a): what each stage's memory claim was
//! sized by and what the stage then took, what each test run's binaries claimed against what they
//! held, and what the run as a whole was and used. They go through the account's one writer
//! ([`aterm_capacity::admission_log::user`]), so each waits for no lock and a lost one is counted,
//! and none fails or delays the run. The capacity ledger's `look` and `admitted` rows name a
//! claim by its id ([`aterm_capacity::admission_log::id_of`]), and a `stage` row names its claim
//! the same way, which is how a reader joins what a waiter waited behind to what the holder's
//! work then took.
//!
//! - `stage`, at each stage's end ([`stage`]): the run, the stage, its claim's id and bytes, what
//!   sized that claim (`basis`) and, where a record keyed by its compiler jobs did, that width
//!   (`keyed_jobs`), the compile width, the stage's peak, whether its record kept that peak, and
//!   the CPU, wall and wait for a core it read. Nothing for a stage that never started. It is
//!   written once the stage's claim is let go, so no claim is held for its write.
//! - `tests`, at each test dispatch's end ([`tests`]): its binaries' claims and peaks summed by
//!   whether the peak each was claimed by was measured at the width it ran at, their CPU and
//!   wait for a core, and the recording pass's claim and peak. One row a run, never one a binary,
//!   which keeps the log near a megabyte a day.
//! - `run`, once the run has reported ([`run`]): its checkout, whether `verify land` judged it,
//!   ran it in the background or `--admit now` forced it, its checks, its start and end, its
//!   stages' CPU and its binaries', and its outcome. A run is one run of the checks
//!   (`crate::run_with`), not one process: a process that runs several (`verify release-input`,
//!   one a check) writes a `run` row for each, each under its own id ([`Tally`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_capacity::admission_log::{Row, id_of};
use aterm_json::Value;

use crate::plan::StageSpec;
use crate::{Ctx, ResourceClaim};

/// The runs this process has begun ([`Tally::begin`]), the last of each run's id.
static RUNS: AtomicU64 = AtomicU64::new(0);

/// THE RUN'S TALLY FOR ITS ROWS: its id and when it began, and the CPU its stages' rows and its
/// test binaries' read, begun afresh by each run of a context's checks ([`Tally::begin`]).
#[derive(Debug, Default)]
pub(crate) struct Tally {
    /// Its id (`<pid>-<start_ms>-<n>`, the `n`th run of this process) and its start, unix ms;
    /// `None` before a run began.
    begun: Mutex<Option<(String, u64)>>,
    /// The CPU its stages' rows read, ms.
    stage_cpu_ms: AtomicU64,
    /// The CPU its test binaries' rows read, ms.
    test_cpu_ms: AtomicU64,
    /// Whether `verify land` judges this process's runs ([`Tally::judged_by_land`]); no run's
    /// beginning clears it.
    land: std::sync::atomic::AtomicBool,
}

impl Tally {
    /// Begin a run: a new id, its CPU counted from nothing. When it began, unix ms.
    pub(crate) fn begin(&self) -> u64 {
        let start_ms = aterm_time::unix_millis().unwrap_or(0);
        let n = RUNS.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("{}-{start_ms}-{n}", std::process::id());
        *self.begun.lock().unwrap_or_else(PoisonError::into_inner) = Some((id, start_ms));
        self.stage_cpu_ms.store(0, Ordering::Relaxed);
        self.test_cpu_ms.store(0, Ordering::Relaxed);
        start_ms
    }

    /// Say that `verify land` judges this process's runs: it was given `--queued-since`, which
    /// only a landing passes, and passes to every judgment and round ([`crate::cli::Args`]).
    pub(crate) fn judged_by_land(&self) {
        self.land.store(true, Ordering::Relaxed);
    }

    /// The run's id; `None` before a run began (a stage run on its own, as a test runs one).
    pub(crate) fn id(&self) -> Option<String> {
        self.begun
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(id, _)| id.clone())
    }
}

/// `d` in whole ms.
fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// The word a `stage` row's `basis` says a claim sized by `how` by ([`crate::memory::need`]).
#[must_use]
pub(crate) const fn how_word(how: &crate::memory::How) -> &'static str {
    use crate::memory::How;
    match how {
        How::Learned(_) => "learned",
        How::Elsewhere(_) => "elsewhere",
        How::Plain(_) => "plain",
        How::WiderElsewhere(_) => "wider-elsewhere",
        How::Composed(_) => "composed",
        How::Cold => "cold",
        How::Light => "light",
        How::Dispatched => "dispatched",
        How::Unmetered => "unmetered",
    }
}

/// WHAT SIZED A STAGE'S CLAIM, for its `stage` row: the word (`learned`, `shaped`, `now`, …), and
/// where the record that sized it is the one keyed by the stage's compiler jobs
/// ([`crate::memory::need_keyed`]), that width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Basis {
    /// The word.
    pub(crate) word: &'static str,
    /// The compiler jobs of the width-keyed record that sized it; `None` where an unkeyed
    /// record, or none, did.
    pub(crate) keyed_jobs: Option<u32>,
}

impl Basis {
    /// The basis `word`, sized by no width-keyed record.
    #[must_use]
    pub(crate) const fn word(word: &'static str) -> Basis {
        Basis {
            word,
            keyed_jobs: None,
        }
    }
}

/// What sized the claim of stage `spec`, whose lookup found `need`, from the record keyed by
/// compiler jobs `keyed_jobs` where one ([`crate::claim_memory`]): `shaped` where the shaper
/// planned it or raised it ([`crate::shape::Part`]), the lookup's own word and width where its
/// figure is the one claimed, else `planned` (the plan's figure, which a record kept since the
/// plan no longer reads as).
#[must_use]
pub(crate) const fn basis(
    spec: &StageSpec,
    need: &crate::memory::StageNeed,
    keyed_jobs: Option<u32>,
) -> Basis {
    if spec.part.is_some() {
        Basis::word("shaped")
    } else if need.bytes == spec.mem {
        Basis {
            word: how_word(&need.how),
            keyed_jobs,
        }
    } else {
        Basis::word("planned")
    }
}

/// The word a `stage` row's `kept` says [`crate::memory::Book::keep`]'s answer by.
#[must_use]
pub(crate) const fn kept_word(kept: &std::io::Result<crate::memory::Kept>) -> &'static str {
    match kept {
        Ok(crate::memory::Kept::Kept) => "kept",
        Ok(crate::memory::Kept::Light) => "light",
        Ok(crate::memory::Kept::LightBeside(_)) => "light-beside",
        Ok(crate::memory::Kept::Nowhere) => "nowhere",
        Err(_) => "error",
    }
}

/// The compile width of a stage of `ctx` whose compiler jobs are `jobs`: that, else the run's
/// parallelism ([`Ctx::parallelism`]: the host's, where no grant narrows the verifier), which an
/// unpooled compile runs at unless a `--jobs` ceiling lowers it.
fn width(ctx: &Ctx, jobs: Option<u32>) -> u32 {
    jobs.unwrap_or_else(|| ctx.parallelism())
}

/// The `stage` row of stage `spec`, begun at `begun`: its claim (`claim`), compiled at `jobs`,
/// metered by `meter`, its peak kept as `kept` says ([`kept_word`], `not-kept` where it was not
/// offered to the record). With no meter it says no peak, CPU or wait; with no claim its basis is
/// `none`, and a claim `--admit now` forced says `now`. Built while the claim is held, it is
/// written once the claim is let go ([`stage`]).
pub(crate) fn stage_row(
    ctx: &Ctx,
    spec: &StageSpec,
    claim: Option<&ResourceClaim>,
    jobs: Option<u32>,
    meter: Option<&crate::memory::Meter>,
    kept: Option<&'static str>,
    begun: Instant,
) -> Row {
    let held = claim.map(|c| &c.claim);
    let basis = match claim {
        None => Basis::word("none"),
        Some(c) if c.claim.forced() => Basis::word("now"),
        Some(c) => c.basis,
    };
    let cpu = meter
        .and_then(crate::memory::Meter::observed_cpu)
        .map(|cpu| cpu.nanoseconds / 1_000_000);
    Row::now("stage")
        .with("run", ctx.admission.id())
        .with("stage", format!("{:?}", spec.id))
        .with("title", spec.title.as_str())
        .with("claim", held.map(|c| id_of(c.record().order())))
        .with("claimed", held.map(|c| c.record().need.mem))
        .with("basis", basis.word)
        .with("keyed_jobs", basis.keyed_jobs)
        .with("width", width(ctx, jobs))
        .with("pooled", jobs.is_some())
        .with("ceiling", ctx.build_jobs_ceiling())
        .with("peak", meter.and_then(crate::memory::Meter::held))
        .with("kept", kept)
        .with("cpu_ms", cpu)
        .with("wall_ms", ms(begun.elapsed()))
        .with(
            "starved_ms",
            meter.and_then(crate::memory::Meter::starved).map(ms),
        )
}

/// Write a stage's `stage` row ([`stage_row`]) of `ctx`'s run through the account's writer, its
/// CPU counted toward the run's.
pub(crate) fn stage(ctx: &Ctx, row: &Row) {
    if let Some(cpu) = row.u64("cpu_ms") {
        ctx.admission.stage_cpu_ms.fetch_add(cpu, Ordering::Relaxed);
    }
    if let Some(writer) = aterm_capacity::admission_log::user() {
        writer.write(row);
    }
}

/// What a test run's recording pass claimed and held, bytes: its `tests` row's last fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Recording {
    /// Its memory claim; `None` with none (no book).
    pub(crate) claim: Option<u64>,
    /// The most its tree held at one read; `None` where unread.
    pub(crate) peak: Option<u64>,
}

/// One width class of a test run's binaries ([`tests_row`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Class {
    claim: u64,
    peak: u64,
    count: u64,
    /// Claims more than 1.3 times their peak.
    over: u64,
    /// Peaks above their claims, and by how much in all.
    under: u64,
    under_bytes: u64,
}

impl Class {
    fn add(&mut self, claim: u64, peak: u64) {
        self.claim = self.claim.saturating_add(claim);
        self.peak = self.peak.saturating_add(peak);
        self.count += 1;
        if u128::from(claim) * 10 > u128::from(peak) * 13 {
            self.over += 1;
        }
        if peak > claim {
            self.under += 1;
            self.under_bytes = self.under_bytes.saturating_add(peak - claim);
        }
    }

    fn value(self) -> Value {
        aterm_json::json!({
            "claim": self.claim, "peak": self.peak, "count": self.count, "over": self.over,
            "under": self.under, "under_bytes": self.under_bytes,
        })
    }
}

/// The width class of a binary of re-run spec `spec` planned at `width` and claiming `claim`,
/// by `history` (a shard's by its parent's row, as the plan reads it,
/// [`crate::test_shards`]): `matched` where a kept sample that read the peak it was claimed by
/// ran at that width, `mismatched` where only samples at other widths read it, `unseen` where no
/// kept sample read it (it claimed the run's fallback).
fn class_of(
    history: &crate::test_history::History,
    spec: &str,
    width: u32,
    claim: u64,
) -> &'static str {
    let row = history
        .row(spec)
        .or_else(|| history.row(spec.split_once(" -- ")?.0));
    let mut set_it = row
        .into_iter()
        .flat_map(|row| &row.samples)
        .filter(|s| s.peak_bytes == Some(claim))
        .peekable();
    if set_it.peek().is_none() {
        "unseen"
    } else if set_it.any(|s| s.width == width) {
        "matched"
    } else {
        "mismatched"
    }
}

/// The `tests` row of test run `label`, of the run `run`: each started binary of `bins` (`ran`
/// beside them) that read its tree's use, its claim and peak summed in its width class
/// ([`class_of`]); every such binary's CPU and wait for a core; and the recording pass's claim
/// and peak.
pub(crate) fn tests_row(
    run: Option<String>,
    label: &str,
    bins: &[crate::testrun::Binary],
    ran: &[Option<crate::testrun::Ran>],
    history: &crate::test_history::History,
    recording: Recording,
) -> Row {
    let mut classes = std::collections::BTreeMap::<&str, Class>::new();
    let (mut cpu, mut starved, mut binaries) = (0_u64, 0_u64, 0_u64);
    for (b, ran) in bins.iter().zip(ran) {
        let Some(ran) = ran.as_ref().filter(|r| r.started) else {
            continue;
        };
        let Some(usage) = ran.run.usage else {
            continue;
        };
        binaries += 1;
        cpu = cpu.saturating_add(ms(usage.cpu));
        starved = starved.saturating_add(ms(usage.starved));
        let claim = ran.planned.peak;
        classes
            .entry(class_of(history, &b.spec, ran.planned.width, claim))
            .or_default()
            .add(claim, usage.peak_bytes);
    }
    let classes: aterm_json::Map = ["matched", "mismatched", "unseen"]
        .into_iter()
        .map(|c| {
            (
                c.to_owned(),
                classes.get(c).copied().unwrap_or_default().value(),
            )
        })
        .collect();
    Row::now("tests")
        .with("run", run)
        .with("title", label)
        .with("classes", Value::Object(classes))
        .with("binaries", binaries)
        .with("cpu_ms", cpu)
        .with("starved_ms", starved)
        .with("recording_claim", recording.claim)
        .with("recording_peak", recording.peak)
}

/// Write test run `label`'s `tests` row ([`tests_row`]) of `ctx`'s run through the account's
/// writer, its CPU counted toward the run's.
pub(crate) fn tests(
    ctx: &Ctx,
    label: &str,
    bins: &[crate::testrun::Binary],
    ran: &[Option<crate::testrun::Ran>],
    history: &crate::test_history::History,
    recording: Recording,
) {
    let row = tests_row(ctx.admission.id(), label, bins, ran, history, recording);
    if let Some(cpu) = row.u64("cpu_ms") {
        ctx.admission.test_cpu_ms.fetch_add(cpu, Ordering::Relaxed);
    }
    if let Some(writer) = aterm_capacity::admission_log::user() {
        writer.write(&row);
    }
}

/// The word a `run` row's `outcome` says exit `code` by.
const fn outcome(code: i32) -> &'static str {
    match code {
        crate::exit::PASS => "pass",
        crate::exit::FAILED => "fail",
        crate::exit::USAGE => "usage",
        crate::exit::COULD_NOT_RUN => "could-not-run",
        _ => "other",
    }
}

/// The `run` row of `ctx`'s run, begun at `start_ms` and ending with `code`: what was asked of
/// it, as `ctx` says. `land`: `verify land` judged it, which is a run given `--queued-since`
/// ([`Tally::judged_by_land`]); `background`
/// ([`Ctx::background`]); `forced`: `--admit now` decides its admissions ([`Ctx::quiet`]).
pub(crate) fn run_row(ctx: &Ctx, start_ms: u64, code: i32) -> Row {
    let checks: Vec<Value> = ctx.checks.iter().map(|c| Value::from(c.name())).collect();
    Row::now("run")
        .with("run", ctx.admission.id())
        .with("checkout", ctx.root.display().to_string())
        .with("land", ctx.admission.land.load(Ordering::Relaxed))
        .with("background", ctx.background)
        .with("forced", ctx.quiet == Some(aterm_capacity::Admit::Now))
        .with("checks", checks)
        .with("start_ms", start_ms)
        .with("end_ms", aterm_time::unix_millis().unwrap_or(0))
        .with(
            "stage_cpu_ms",
            ctx.admission.stage_cpu_ms.load(Ordering::Relaxed),
        )
        .with(
            "test_cpu_ms",
            ctx.admission.test_cpu_ms.load(Ordering::Relaxed),
        )
        .with("outcome", outcome(code))
        .with("code", code)
}

/// Write the `run` row of `ctx`'s run ([`run_row`]) through the account's writer.
pub(crate) fn run(ctx: &Ctx, start_ms: u64, code: i32) {
    if let Some(writer) = aterm_capacity::admission_log::user() {
        writer.write(&run_row(ctx, start_ms, code));
    }
}

#[cfg(test)]
#[path = "admission_rows_tests.rs"]
mod tests;
