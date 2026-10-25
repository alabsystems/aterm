// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spin matrix: an idle instance must stay idle.

use std::time::Duration;

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, uint_field,
};
use super::starvation::ShapeVerdict;

/// What sits on the instance while it is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinScene {
    /// The steady repainter.
    ClaudeShaped,
    /// A bare `/bin/sh` prompt: the floor.
    Idle,
    /// A recording `/bin/sh` prompt frozen in an open Rewind view.
    Rewound,
}

/// One spin row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpinRow {
    pub scene: SpinScene,
    /// The measured interval, opening as the client starts.
    pub window: Duration,
    pub max_arms: u64,
    pub max_timer_wakes: Option<u64>,
}

const SETTLE_QUIET_MS: &str = "3000";

/// A take's wall-clock limit: every wait and request on the instance ends with it.
pub const BUDGET_SECS: u64 = 90;

/// One row's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpinReport {
    pub row: SpinRow,
    pub past_deadline_arms: u64,
    pub stale_arm_heals: Option<u64>,
    pub frames: Option<u64>,
    pub timer_wakes: Option<u64>,
    /// The status reasons at the end of the window.
    pub reasons: Option<String>,
    pub verdict: ShapeVerdict,
}

impl std::fmt::Display for SpinReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SPIN scene={:?} window={:?} past_deadline_arms={} max_arms={} stale_arm_heals={:?} \
             frames={:?} timer_wakes={:?} max_timer_wakes={:?} reasons={:?} verdict={:?}",
            self.row.scene,
            self.row.window,
            self.past_deadline_arms,
            self.row.max_arms,
            self.stale_arm_heals,
            self.frames,
            self.timer_wakes,
            self.row.max_timer_wakes,
            self.reasons,
            self.verdict
        )
    }
}

/// The row's bounds against the window's counters.
#[must_use]
pub const fn judge(row: &SpinRow, arms: u64, timer_wakes: Option<u64>) -> ShapeVerdict {
    let wakes_ok = match (row.max_timer_wakes, timer_wakes) {
        (None, _) => true,
        (Some(max), Some(n)) => n <= max,
        (Some(_), None) => false,
    };
    if arms <= row.max_arms && wakes_ok {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when the instance did not start, a counter is missing, or
/// the claude scene was not the shape it claims (off the alt screen, or its
/// output moved the grid).
pub fn measure(rig: &Rig, row: &SpinRow) -> Result<SpinReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    super::instance::first_exec(&rig.fake_tui);
    let budget = Budget::from_now(BUDGET_SECS);
    let mut scratch = Scratch::new("atspin")
        .map_err(|e| CouldNotRun(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let head = match row.scene {
        SpinScene::Rewound => "temporal_recording = true",
        SpinScene::ClaudeShaped | SpinScene::Idle => "# effect settings: the shipped defaults",
    };
    let world = World::prepare(scratch.path(), head)
        .map_err(|e| CouldNotRun(format!("cannot lay out the scratch world: {e}")))?;
    let inst = Instance::launch(&rig.aterm, &world, budget)?;
    let ctl = &inst.ctl;
    ctl.ok(&["await", "idle", SETTLE_QUIET_MS, "timeout=60000"])?;
    if row.scene == SpinScene::Rewound {
        ctl.ok(&["send", "printf 'rewind spin\\n'"])?;
        ctl.ok(&["key", "enter"])?;
        ctl.ok(&["await", "idle", SETTLE_QUIET_MS, "timeout=60000"])?;
        let open = ctl.ok(&["rewind", "open"])?;
        if field(&open, "view") != Some("open") {
            return could_not_run(format!("the Rewind view did not open ({open})"));
        }
    }
    ctl.ok(&["metrics", "reset"])?;
    let before = ctl
        .field(&["metrics"], "past_deadline_arms")?
        .parse::<u64>()
        .map_err(|_| CouldNotRun("past_deadline_arms is not a count".into()))?;
    if row.scene == SpinScene::ClaudeShaped {
        let launch = format!("'{}' --shape repainter", rig.fake_tui.display());
        ctl.ok(&["send", &launch])?;
        ctl.ok(&["key", "enter"])?;
    }
    std::thread::sleep(row.window);
    let mut reasons = None;
    if row.scene == SpinScene::ClaudeShaped {
        let modes = ctl.ok(&["modes"])?;
        if field(&modes, "alt_screen") != Some("true") {
            return could_not_run(format!(
                "the claude scene never reached the alt screen ({modes})"
            ));
        }
        let status = ctl.ok(&["status"])?;
        let why = field(&status, "reasons").unwrap_or("").to_string();
        if !why.split(',').any(|r| r == "output_activity") {
            return could_not_run(format!(
                "the claude scene is not output without movement (reasons={why})"
            ));
        }
        reasons = Some(why);
    }
    if row.scene == SpinScene::Rewound {
        let status = ctl.ok(&["rewind", "status"])?;
        if field(&status, "view") != Some("open") {
            return could_not_run(format!("the Rewind view closed in the window ({status})"));
        }
    }
    let metrics = ctl.ok(&["metrics"])?;
    let after = uint_field(&metrics, "past_deadline_arms")
        .ok_or_else(|| CouldNotRun("ctl metrics stopped reporting past_deadline_arms".into()))?;
    let timer_wakes = uint_field(&metrics, "timer_wakes");
    if row.max_timer_wakes.is_some() && timer_wakes.is_none() {
        return could_not_run("ctl metrics does not report timer_wakes");
    }
    let arms = after.saturating_sub(before);
    Ok(SpinReport {
        row: *row,
        past_deadline_arms: arms,
        stale_arm_heals: uint_field(&metrics, "stale_arm_heals"),
        frames: uint_field(&metrics, "frames"),
        timer_wakes,
        reasons,
        verdict: judge(row, arms, timer_wakes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: SpinRow = SpinRow {
        scene: SpinScene::ClaudeShaped,
        window: Duration::from_secs(10),
        max_arms: 100,
        max_timer_wakes: Some(12),
    };

    #[test]
    fn a_healthy_window_passes_and_either_witness_fails_it() {
        assert_eq!(judge(&CLAUDE, 0, Some(5)), ShapeVerdict::Pass);
        assert_eq!(
            judge(&CLAUDE, 101, Some(5)),
            ShapeVerdict::Fail,
            "the spin itself"
        );
        assert_eq!(
            judge(&CLAUDE, 0, Some(21)),
            ShapeVerdict::Fail,
            "the clamped poll"
        );
    }

    #[test]
    fn a_bounded_wake_count_that_was_not_reported_is_not_a_pass() {
        assert_eq!(judge(&CLAUDE, 0, None), ShapeVerdict::Fail);
        let idle = SpinRow {
            scene: SpinScene::Idle,
            max_timer_wakes: None,
            ..CLAUDE
        };
        assert_eq!(judge(&idle, 0, None), ShapeVerdict::Pass);
    }
}
