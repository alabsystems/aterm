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
}

impl std::fmt::Display for AosReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let max = self.main_us.iter().max().copied().unwrap_or(0);
        write!(
            f,
            "AOS repairs={} keys={} p99_main_us={} max_main_us={max} max_p99_us={} \
             off_main={} verdict={:?}",
            self.main_us.len(),
            self.row.keys,
            self.p99_us,
            self.row.max_p99_us,
            self.off_main,
            self.verdict
        )
    }
}

/// The nearest-rank 99th percentile of `samples` (0 for none).
#[must_use]
pub fn p99(samples: &[u64]) -> u64 {
    let mut s = samples.to_vec();
    s.sort_unstable();
    let rank = (s.len() * 99).div_ceil(100);
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

/// Measure one row: attach the shipped code, then type and repair `row.repairs` times, reading each repair's `main_us=` and `off_main=`.
/// # Errors
/// [`CouldNotRun`] when the instance did not start or a request failed; [`CouldNotRun::Unanswered`] when a repair answered without the fields.
pub fn measure(rig: &Rig, row: &AosRow) -> Result<AosReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = Budget::from_now(BUDGET_SECS);
    let mut scratch = Scratch::new("ataos")
        .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let world = World::prepare(scratch.path(), GLOW_ON)
        .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
    std::fs::write(world.home.join(".shrc"), TYPED_INTO)
        .map_err(|e| CouldNotRun::Refused(format!("cannot write the scratch shell's rc: {e}")))?;
    let inst = Instance::launch(&rig.aterm, &world, budget)?;
    let ctl = &inst.ctl;
    let attached = ctl.ok(&["aos", "attach", "code=shipped"])?;
    if field(&attached, "attached") != Some("true") {
        return could_not_run(format!("aos attach did not attach ({attached})"));
    }
    let mut main_us = Vec::new();
    let mut off_main = true;
    for _ in 0..row.repairs {
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
        main_us.push(us);
        off_main &= off == "true";
    }
    let _ = ctl.ok(&["aos", "detach"]);
    let p99_us = p99(&main_us);
    Ok(AosReport {
        row: *row,
        verdict: judge(row, &main_us, off_main),
        main_us,
        off_main,
        p99_us,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: AosRow = AosRow {
        repairs: 4,
        keys: 8,
        max_p99_us: 8_000,
    };

    #[test]
    fn the_nearest_rank_p99_is_the_top_sample_below_a_hundred_and_ignores_order() {
        assert_eq!(p99(&[]), 0);
        assert_eq!(p99(&[7]), 7);
        assert_eq!(p99(&[5, 1, 9, 3]), 9);
        let hundred: Vec<u64> = (1..=100).rev().collect();
        assert_eq!(p99(&hundred), 99);
        let two_hundred: Vec<u64> = (1..=200).collect();
        assert_eq!(p99(&two_hundred), 198);
    }

    #[test]
    fn a_fast_off_main_matrix_passes_and_each_witness_fails_it() {
        assert_eq!(
            judge(&ROW, &[900, 1200, 800, 1000], true),
            ShapeVerdict::Pass
        );
        assert_eq!(
            judge(&ROW, &[900, 1200, 800, 114_500], true),
            ShapeVerdict::Fail,
            "proven red: R0's main-thread repair (about 114.5 ms)"
        );
        assert_eq!(
            judge(&ROW, &[900, 1200, 800, 1000], false),
            ShapeVerdict::Fail,
            "a prepare on the main thread"
        );
        assert_eq!(
            judge(&ROW, &[900, 1200, 800], true),
            ShapeVerdict::Fail,
            "a repair not measured"
        );
        assert_eq!(
            judge(&AosRow { repairs: 0, ..ROW }, &[], true),
            ShapeVerdict::Fail
        );
    }
}
