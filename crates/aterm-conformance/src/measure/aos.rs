// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aos matrix: the main-thread time an `aos repair` takes on the release binary.

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, unanswered,
};
use super::starvation::ShapeVerdict;

/// One aos row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AosRow {
    /// Repairs measured.
    pub repairs: u32,
    /// Keys typed before each repair, so each one replays a recorded trace.
    pub keys: u32,
    /// The most main-thread time the 99th-percentile repair may take, in microseconds.
    pub max_p99_us: u64,
}

/// A take's wall-clock limit: every wait and request on the instance ends with it.
pub const BUDGET_SECS: u64 = 300;

/// The fewest repairs before an irreversible failure can end acquisition. A passing p99
/// still takes the entire declared sample; this finite-sample row makes no population claim.
const MIN_REPAIRS: usize = 8;

/// The glow on, so a typed key reaches the hosted task.
const GLOW_ON: &str = "serious_mode = false\ncursor_trail = true\ncursor_momentum_glow = true";

/// The shell's rc: the keys go to `cat` on a tty that echoes them, since a recording stops at a key a program reads raw outside an integrated prompt.
const TYPED_INTO: &str = "exec /bin/cat\n";

/// One row's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AosReport {
    pub row: AosRow,
    /// Each repair's `main_us=`, in order.
    pub main_us: Vec<u64>,
    /// Whether every repair's prepare ran off the main thread (`off_main=true`).
    pub off_main: bool,
    pub p99_us: u64,
    pub verdict: ShapeVerdict,
    /// The session's replay checks at the end ([`super::instance::replay_checks`]).
    pub checks: String,
}

impl std::fmt::Display for AosReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let max = self.main_us.iter().max().copied().unwrap_or(0);
        write!(
            f,
            "AOS repairs={} planned_repairs={} stopping_sample={} decision=all_completions \
             keys={} p99_main_us={} max_main_us={max} max_p99_us={} \
             off_main={} verdict={:?} {}",
            self.main_us.len(),
            self.row.repairs,
            self.main_us.len(),
            self.row.keys,
            self.p99_us,
            self.row.max_p99_us,
            self.off_main,
            self.verdict,
            self.checks
        )
    }
}

/// The nearest-rank 99th percentile of `samples` (0 for none).
#[must_use]
pub fn p99(samples: &[u64]) -> u64 {
    let mut s = samples.to_vec();
    s.sort_unstable();
    let rank = ((s.len() as u128) * 99).div_ceil(100) as usize;
    rank.checked_sub(1)
        .and_then(|i| s.get(i))
        .copied()
        .unwrap_or(0)
}

/// The row's bound: every repair measured, each prepared off the main thread, and the p99 within the bound.
#[must_use]
pub fn judge(row: &AosRow, main_us: &[u64], off_main: bool) -> ShapeVerdict {
    let counted = u32::try_from(main_us.len()).is_ok_and(|n| n == row.repairs && n > 0);
    if counted && off_main && p99(main_us) <= row.max_p99_us {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

/// A failure that no remaining repair can reverse. A favorable prefix never passes:
/// the unobserved tail and every repair's prepare-thread check remain obligations.
#[must_use]
pub fn decision(row: &AosRow, main_us: &[u64], off_main: bool) -> Option<ShapeVerdict> {
    let cap = row.repairs as usize;
    if main_us.len() < cap.min(MIN_REPAIRS) {
        return None;
    }
    let rank = cap - cap / 100;
    let (lo, _) = super::decision::completion(main_us, cap, rank)?;
    if !off_main || lo > row.max_p99_us {
        Some(ShapeVerdict::Fail)
    } else if main_us.len() == cap {
        Some(judge(row, main_us, off_main))
    } else {
        None
    }
}

fn run_plan(
    row: &AosRow,
    mut sample: impl FnMut(u32) -> Result<(u64, bool), CouldNotRun>,
) -> Result<(Vec<u64>, bool), CouldNotRun> {
    let mut main_us = Vec::new();
    let mut off_main = true;
    for repair in 0..row.repairs {
        let (us, off) = sample(repair)?;
        main_us.push(us);
        off_main &= off;
        if decision(row, &main_us, off_main).is_some() {
            break;
        }
    }
    Ok((main_us, off_main))
}

/// Measure one row: attach the shipped code, then type and repair at most `row.repairs` times,
/// reading each repair's `main_us=` and `off_main=`. Only an irreversible failure ends early.
/// # Errors
/// [`CouldNotRun`] when the instance did not start or a request failed; [`CouldNotRun::Unanswered`] when a repair answered without the fields.
pub fn measure(rig: &Rig, row: &AosRow) -> Result<AosReport, CouldNotRun> {
    let _one = super::take(true);
    let mut phases = super::instance::Phases::new("row=aos".into());
    let budget = Budget::from_now(BUDGET_SECS);
    let mut scratch = Scratch::new("ataos")
        .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let world = World::prepare(scratch.path(), GLOW_ON)
        .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
    super::write_fresh(world.home.join(".shrc"), TYPED_INTO)
        .map_err(|e| CouldNotRun::Refused(format!("cannot write the scratch shell's rc: {e}")))?;
    let inst = Instance::launch(&rig.aterm, &world, budget)?;
    phases.end("launch");
    let ctl = &inst.ctl;
    let attached = ctl.ok(&["aos", "attach", "code=shipped"])?;
    if field(&attached, "attached") != Some("true") {
        return could_not_run(format!("aos attach did not attach ({attached})"));
    }
    phases.end("attach");
    let (main_us, off_main) = run_plan(row, |repair| {
        let started = std::time::Instant::now();
        for _ in 0..row.keys {
            ctl.ok(&["key", "a"])?;
        }
        ctl.ok(&["key", "enter"])?;
        let line = ctl.ok(&["aos", "repair", "code=shipped"])?;
        let us = field(&line, "main_us").and_then(|v| v.parse::<u64>().ok());
        let off = field(&line, "off_main");
        let (Some(us), Some(off)) = (us, off) else {
            return unanswered(format!(
                "aos repair named no main_us= or off_main= ({line})"
            ));
        };
        eprintln!(
            "measure-sample: {}",
            aterm_json::json!({
                "schema": 1, "measurement": "aos", "sample": repair + 1,
                "main_us": us, "off_main": off == "true", "seconds": started.elapsed().as_secs_f64(),
            })
        );
        Ok((us, off == "true"))
    })?;
    phases.end("samples");
    let _ = ctl.ok(&["aos", "detach"]);
    let p99_us = p99(&main_us);
    let report = AosReport {
        checks: super::instance::replay_checks(ctl),
        row: *row,
        verdict: judge(row, &main_us, off_main),
        main_us,
        off_main,
        p99_us,
    };
    phases.end("fences");
    drop(inst);
    phases.end("stop");
    Ok(report)
}

#[cfg(test)]
#[path = "aos_tests.rs"]
mod tests;
