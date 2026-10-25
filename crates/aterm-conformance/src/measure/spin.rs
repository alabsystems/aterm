// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spin matrix: an idle instance must stay idle. A row bounds every wake its loop takes
//! (`event_wakes`: timer, wait-cancelled and poll wakes), not only its timers: a loop that wakes
//! itself as it parks counts wait-cancelled wakes and turns, never a timer wake.

use std::time::Duration;

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Instance, Scratch, World, could_not_run, field, uint_field, unanswered,
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
    /// A bare `/bin/sh` prompt in a window: AppKit's loop and its proxies, which a headless
    /// instance never runs. Under `motion = "reduced"`: a focused window's motion (the Deck's live
    /// edge, the pet) wakes it many times a second, and the rig does not decide whether the window
    /// it opens holds the focus.
    Windowed,
}

/// One spin row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpinRow {
    pub scene: SpinScene,
    /// The measured interval, opening as the client starts.
    pub window: Duration,
    pub max_arms: u64,
    pub max_timer_wakes: Option<u64>,
    /// Every loop wake in the window (`event_wakes`).
    pub max_wakes: u64,
}

const SETTLE_QUIET_MS: &str = "3000";

/// The most window events beyond its redraws a windowed take may see and still be idle: a focus
/// or occlusion change is one or two, and a pointer over the window or a key typed into it is
/// many, which make the take a refusal, never a finding.
pub const MAX_OS_EVENTS: u64 = 30;

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
    /// Every loop wake in the window (`event_wakes`).
    pub wakes: Option<u64>,
    pub wait_cancelled_wakes: Option<u64>,
    pub turns: Option<u64>,
    /// The window's `wakes_by_variant`: the proxy wakes that came with an event.
    pub wakes_by_variant: Option<String>,
    /// The window's `deadline_fires_by_owner`: whose deadline each timer wake was.
    pub fires_by_owner: Option<String>,
    /// A windowed take's window events beyond its redraws (at most [`MAX_OS_EVENTS`]).
    pub os_events: Option<u64>,
    /// The status reasons at the end of the window.
    pub reasons: Option<String>,
    pub verdict: ShapeVerdict,
}

impl std::fmt::Display for SpinReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SPIN scene={:?} window={:?} past_deadline_arms={} max_arms={} stale_arm_heals={:?} \
             frames={:?} timer_wakes={:?} max_timer_wakes={:?} wakes={:?} max_wakes={} \
             wait_cancelled_wakes={:?} turns={:?} wakes_by_variant={:?} fires_by_owner={:?} \
             os_events={:?} reasons={:?} verdict={:?}",
            self.row.scene,
            self.row.window,
            self.past_deadline_arms,
            self.row.max_arms,
            self.stale_arm_heals,
            self.frames,
            self.timer_wakes,
            self.row.max_timer_wakes,
            self.wakes,
            self.row.max_wakes,
            self.wait_cancelled_wakes,
            self.turns,
            self.wakes_by_variant,
            self.fires_by_owner,
            self.os_events,
            self.reasons,
            self.verdict
        )
    }
}

/// The row's bounds against the window's counters: past-deadline arms, timer wakes, and every
/// loop wake (`wakes`), which an unreported count never passes.
#[must_use]
pub const fn judge(
    row: &SpinRow,
    arms: u64,
    timer_wakes: Option<u64>,
    wakes: Option<u64>,
) -> ShapeVerdict {
    let timers_ok = match (row.max_timer_wakes, timer_wakes) {
        (None, _) => true,
        (Some(max), Some(n)) => n <= max,
        (Some(_), None) => false,
    };
    let wakes_ok = match wakes {
        Some(n) => n <= row.max_wakes,
        None => false,
    };
    if arms <= row.max_arms && timers_ok && wakes_ok {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

/// `root`'s booked turns in a `metrics percentiles` line's `turns_by_root` (`root:n/p50/p95/p99`
/// pairs); 0 for a root that booked none.
#[must_use]
pub fn root_turns(line: &str, root: &str) -> Option<u64> {
    let roots = field(line, "turns_by_root")?;
    if roots == "none" {
        return Some(0);
    }
    Some(
        roots
            .split(',')
            .find_map(|pair| pair.strip_prefix(root)?.strip_prefix(':'))
            .and_then(|rec| rec.split('/').next()?.parse().ok())
            .unwrap_or(0),
    )
}

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when the instance did not start, a counter is missing, the
/// claude scene was not the shape it claims (off the alt screen, or its output
/// moved the grid), or the window took more than [`MAX_OS_EVENTS`] events
/// beyond its redraws (a person's pointer or keys: not idle).
pub fn measure(rig: &Rig, row: &SpinRow) -> Result<SpinReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    super::instance::first_exec(&rig.fake_tui);
    let budget = Budget::from_now(BUDGET_SECS);
    let mut scratch = Scratch::new("atspin")
        .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let head = match row.scene {
        SpinScene::Rewound => "temporal_recording = true",
        SpinScene::Windowed => "motion = \"reduced\"",
        SpinScene::ClaudeShaped | SpinScene::Idle => "# effect settings: the shipped defaults",
    };
    let world = World::prepare(scratch.path(), head)
        .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
    let inst = if row.scene == SpinScene::Windowed {
        Instance::launch_windowed(&rig.aterm, &world, budget)?
    } else {
        Instance::launch(&rig.aterm, &world, budget)?
    };
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
        .map_err(|_| CouldNotRun::Refused("past_deadline_arms is not a count".into()))?;
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
    let mut os_events = None;
    if row.scene == SpinScene::Windowed {
        let roots = ctl.ok(&["metrics", "percentiles"])?;
        let events = root_turns(&roots, "window_event").ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics percentiles does not report turns_by_root".into())
        })?;
        let redraws = uint_field(&metrics, "redraw_attempts").ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics does not report redraw_attempts".into())
        })?;
        let input = events.saturating_sub(redraws);
        os_events = Some(input);
        if input > MAX_OS_EVENTS {
            return could_not_run(format!(
                "the window took {input} events beyond its {redraws} redraws in {:?} ({events} \
                 window_event turns): a pointer or keys reached it, so it was not idle",
                row.window
            ));
        }
    }
    let after = uint_field(&metrics, "past_deadline_arms").ok_or_else(|| {
        CouldNotRun::Refused("ctl metrics stopped reporting past_deadline_arms".into())
    })?;
    let timer_wakes = uint_field(&metrics, "timer_wakes");
    if row.max_timer_wakes.is_some() && timer_wakes.is_none() {
        return unanswered("ctl metrics does not report timer_wakes");
    }
    let wakes = uint_field(&metrics, "event_wakes");
    if wakes.is_none() {
        return unanswered("ctl metrics does not report event_wakes");
    }
    let arms = after.saturating_sub(before);
    Ok(SpinReport {
        row: *row,
        past_deadline_arms: arms,
        stale_arm_heals: uint_field(&metrics, "stale_arm_heals"),
        frames: uint_field(&metrics, "frames"),
        timer_wakes,
        wakes,
        wait_cancelled_wakes: uint_field(&metrics, "wait_cancelled_wakes"),
        turns: uint_field(&metrics, "turns"),
        wakes_by_variant: field(&metrics, "wakes_by_variant").map(str::to_string),
        fires_by_owner: field(&metrics, "deadline_fires_by_owner").map(str::to_string),
        os_events,
        reasons,
        verdict: judge(row, arms, timer_wakes, wakes),
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
        max_wakes: 400,
    };

    #[test]
    fn a_healthy_window_passes_and_any_witness_fails_it() {
        assert_eq!(judge(&CLAUDE, 0, Some(5), Some(200)), ShapeVerdict::Pass);
        assert_eq!(
            judge(&CLAUDE, 101, Some(5), Some(200)),
            ShapeVerdict::Fail,
            "the spin itself"
        );
        assert_eq!(
            judge(&CLAUDE, 0, Some(21), Some(200)),
            ShapeVerdict::Fail,
            "the clamped poll"
        );
        assert_eq!(
            judge(&CLAUDE, 0, Some(5), Some(401)),
            ShapeVerdict::Fail,
            "a loop that wakes itself: no arm, no timer"
        );
    }

    #[test]
    fn a_bounded_wake_count_that_was_not_reported_is_not_a_pass() {
        assert_eq!(judge(&CLAUDE, 0, None, Some(200)), ShapeVerdict::Fail);
        assert_eq!(judge(&CLAUDE, 0, Some(5), None), ShapeVerdict::Fail);
        let idle = SpinRow {
            scene: SpinScene::Idle,
            max_timer_wakes: None,
            ..CLAUDE
        };
        assert_eq!(judge(&idle, 0, None, Some(200)), ShapeVerdict::Pass);
        assert_eq!(judge(&idle, 0, None, None), ShapeVerdict::Fail);
    }

    #[test]
    fn a_roots_turns_are_read_from_its_pair_and_an_absent_root_booked_none() {
        let line = "OK n_input=0 turns_by_root=window_event:161/3.93/10.49/11.53,\
                    user_event:171/0.02/0.02/0.03,new_events:331/0.07/0.23/0.33 n_render=161";
        assert_eq!(root_turns(line, "window_event"), Some(161));
        assert_eq!(root_turns(line, "new_events"), Some(331));
        assert_eq!(root_turns(line, "modal"), Some(0));
        assert_eq!(root_turns("OK turns_by_root=none", "window_event"), Some(0));
        assert_eq!(root_turns("OK n_input=0", "window_event"), None);
    }
}
