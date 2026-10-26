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
    /// [`Self::Windowed`] at full motion under the shipped cursor style, the rainbow kitty pet,
    /// measured once the pet is in deep sleep ([`PET_DEEP_SLEEP`]). A pet asleep past its breath
    /// window takes no frame and offers no deadline, and a window without the focus neither
    /// animates its pet nor asks it for one: either way the cursor-effect lane fires none,
    /// whichever focus the window gets.
    PetAsleep,
}

impl SpinScene {
    /// Whether the scene opens a window.
    #[must_use]
    pub const fn windowed(self) -> bool {
        matches!(self, Self::Windowed | Self::PetAsleep)
    }
}

/// One spin row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpinRow {
    pub scene: SpinScene,
    /// The measured interval, opening as the client starts.
    pub window: Duration,
    pub max_arms: u64,
    pub max_timer_wakes: Option<u64>,
    /// The cursor-effect lane's timer wakes in the window (`deadline_fires_by_owner`'s
    /// [`EFFECT_LANE`]): the pet's frame train and offers, and the glow's and trail's.
    pub max_effect_fires: Option<u64>,
    /// Every loop wake in the window (`event_wakes`).
    pub max_wakes: u64,
}

const SETTLE_QUIET_MS: &str = "3000";

/// The deadline owner the cursor effects (the pet, the glow, the trail) fire as.
pub const EFFECT_LANE: &str = "cursor_effect";

/// How long the pet scene waits on a quiet prompt before its window opens: past the pet's deep
/// sleep, which `kitty_pet.rs` begins `SLEEP_AFTER` (22 s) and `BREATH_WINDOW` (10 s) after the
/// caret last moved (a lab instance showed it about 34 s after the last key), with room.
pub const PET_DEEP_SLEEP: Duration = Duration::from_secs(40);

/// The cursor style the pet scene draws: the shipped default (`DEFAULT_CURSOR_TRAIL_STYLE`),
/// named so the scene keeps the pet if that default moves.
const PET_STYLE: &str = "rainbow kitty pet";

/// The `trail status` fields a pet take records: the style, the focus and motion its effects saw,
/// and the pet's state.
const PET_FIELDS: [&str; 6] = [
    "resolved",
    "focused",
    "motion_stage",
    "pet_active",
    "pet_action",
    "pet_pose",
];

/// What a pet take's `trail status` must read for its bound to decide anything: the pet drawn, in
/// a focused window at full motion, asleep. A window without the focus moves its pet at reduced
/// motion and asks it for no frame, so a 0 read there passes any pet.
const PET_READY: [(&str, &str); 5] = [
    ("resolved", "rainbow-kitty"),
    ("focused", "true"),
    ("motion_stage", "full"),
    ("pet_active", "true"),
    ("pet_action", "sleep"),
];

/// The first [`PET_READY`] field a `trail status` reply does not read as a pet take needs, as
/// `key=value` (`?` for one the reply lacks); `None` when the take can decide.
fn pet_unready(line: &str) -> Option<String> {
    PET_READY
        .iter()
        .find(|(key, want)| field(line, key) != Some(*want))
        .map(|(key, _)| format!("{key}={}", field(line, key).unwrap_or("?")))
}

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
    /// The cursor-effect lane's timer wakes in the window ([`EFFECT_LANE`]).
    pub effect_fires: Option<u64>,
    /// A pet take's [`PET_FIELDS`] at the end of the window.
    pub pet: Option<String>,
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
             effect_fires={:?} max_effect_fires={:?} wait_cancelled_wakes={:?} turns={:?} \
             wakes_by_variant={:?} fires_by_owner={:?} os_events={:?} pet={:?} reasons={:?} \
             verdict={:?}",
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
            self.effect_fires,
            self.row.max_effect_fires,
            self.wait_cancelled_wakes,
            self.turns,
            self.wakes_by_variant,
            self.fires_by_owner,
            self.os_events,
            self.pet,
            self.reasons,
            self.verdict
        )
    }
}

/// The window's counters a row judges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    pub arms: u64,
    pub timer_wakes: Option<u64>,
    pub effect_fires: Option<u64>,
    pub wakes: Option<u64>,
}

/// `n` within an optional bound `max`: no bound passes anything, and a bounded count that was
/// not reported passes none.
const fn within(max: Option<u64>, n: Option<u64>) -> bool {
    match (max, n) {
        (None, _) => true,
        (Some(max), Some(n)) => n <= max,
        (Some(_), None) => false,
    }
}

/// The row's bounds against the window's counters: past-deadline arms, timer wakes, the
/// cursor-effect lane's fires, and every loop wake (`wakes`), which an unreported count never
/// passes.
#[must_use]
pub const fn judge(row: &SpinRow, c: Counts) -> ShapeVerdict {
    if c.arms <= row.max_arms
        && within(row.max_timer_wakes, c.timer_wakes)
        && within(row.max_effect_fires, c.effect_fires)
        && within(Some(row.max_wakes), c.wakes)
    {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
}

/// `root`'s booked turns in a `metrics percentiles` line's `turns_by_root` (`root:n/p50/p95/p99`
/// pairs); 0 for a root that booked none.
#[must_use]
pub fn root_turns(line: &str, root: &str) -> Option<u64> {
    pair_count(line, "turns_by_root", root)
}

/// `owner`'s timer wakes in a `metrics` line's `deadline_fires_by_owner` (`owner:n` pairs); 0 for
/// an owner that fired none.
#[must_use]
pub fn owner_fires(line: &str, owner: &str) -> Option<u64> {
    pair_count(line, "deadline_fires_by_owner", owner)
}

/// The count `name` leads its record with in the `key` field's `name:n…` pairs (`none` naming
/// none); 0 for a name the field leaves out, `None` when the line has no such field.
fn pair_count(line: &str, key: &str, name: &str) -> Option<u64> {
    let pairs = field(line, key)?;
    if pairs == "none" {
        return Some(0);
    }
    Some(
        pairs
            .split(',')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rec| rec.split('/').next()?.parse().ok())
            .unwrap_or(0),
    )
}

/// The [`PET_FIELDS`] of a `trail status` reply, space-separated.
fn pet_fields(line: &str) -> String {
    PET_FIELDS
        .iter()
        .map(|key| format!("{key}={}", field(line, key).unwrap_or("?")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Measure one row.
/// # Errors
/// [`CouldNotRun`] when the instance did not start, a counter is missing, the
/// claude scene was not the shape it claims (off the alt screen, or its output
/// moved the grid), the window took more than [`MAX_OS_EVENTS`] events
/// beyond its redraws (a person's pointer or keys: not idle), or the pet scene's
/// window was not a focused window's sleeping pet at full motion ([`PET_READY`]).
pub fn measure(rig: &Rig, row: &SpinRow) -> Result<SpinReport, CouldNotRun> {
    let _one = super::ONE_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    super::instance::first_exec(&rig.fake_tui);
    let settle = if row.scene == SpinScene::PetAsleep {
        PET_DEEP_SLEEP
    } else {
        Duration::ZERO
    };
    let budget = Budget::from_now(BUDGET_SECS + settle.as_secs());
    let mut scratch = Scratch::new("atspin")
        .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let head = match row.scene {
        SpinScene::Rewound => "temporal_recording = true".to_string(),
        SpinScene::Windowed => "motion = \"reduced\"".to_string(),
        // `full`, not the shipped `auto`: what `auto` resolves to on a Mac whose Reduce Motion is
        // off, so the take's pet animates whatever this Mac's accessibility setting says.
        SpinScene::PetAsleep => {
            format!("motion = \"full\"\ncursor_trail_style = \"{PET_STYLE}\"")
        }
        SpinScene::ClaudeShaped | SpinScene::Idle => {
            "# effect settings: the shipped defaults".to_string()
        }
    };
    let world = World::prepare(scratch.path(), &head)
        .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
    let inst = if row.scene.windowed() {
        Instance::launch_windowed(&rig.aterm, &world, budget)?
    } else {
        Instance::launch(&rig.aterm, &world, budget)?
    };
    let ctl = &inst.ctl;
    ctl.ok(&["await", "idle", SETTLE_QUIET_MS, "timeout=60000"])?;
    std::thread::sleep(settle);
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
    let mut pet = None;
    if row.scene == SpinScene::PetAsleep {
        let trail = ctl.call(&["trail", "status"]);
        if !trail.ok() {
            return could_not_run(format!(
                "the pet scene's window never held the focus, so its pet asked for nothing \
                 (trail status: {} {})",
                trail.stdout.trim(),
                trail.stderr.trim()
            ));
        }
        let fields = pet_fields(&trail.stdout);
        if let Some(off) = pet_unready(&trail.stdout) {
            return could_not_run(format!(
                "the pet scene's window read {off}, not a focused window's sleeping pet at full \
                 motion ({fields})"
            ));
        }
        pet = Some(fields);
    }
    let mut os_events = None;
    if row.scene.windowed() {
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
    let effect_fires = owner_fires(&metrics, EFFECT_LANE);
    if row.max_effect_fires.is_some() && effect_fires.is_none() {
        return unanswered("ctl metrics does not report deadline_fires_by_owner");
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
        effect_fires,
        pet,
        os_events,
        reasons,
        verdict: judge(
            row,
            Counts {
                arms,
                timer_wakes,
                effect_fires,
                wakes,
            },
        ),
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
        max_effect_fires: None,
        max_wakes: 400,
    };

    const PET: SpinRow = SpinRow {
        scene: SpinScene::PetAsleep,
        max_timer_wakes: None,
        max_effect_fires: Some(0),
        ..CLAUDE
    };

    const fn counts(arms: u64, timer_wakes: Option<u64>, wakes: Option<u64>) -> Counts {
        Counts {
            arms,
            timer_wakes,
            effect_fires: None,
            wakes,
        }
    }

    #[test]
    fn a_healthy_window_passes_and_any_witness_fails_it() {
        assert_eq!(
            judge(&CLAUDE, counts(0, Some(5), Some(200))),
            ShapeVerdict::Pass
        );
        assert_eq!(
            judge(&CLAUDE, counts(101, Some(5), Some(200))),
            ShapeVerdict::Fail,
            "the spin itself"
        );
        assert_eq!(
            judge(&CLAUDE, counts(0, Some(21), Some(200))),
            ShapeVerdict::Fail,
            "the clamped poll"
        );
        assert_eq!(
            judge(&CLAUDE, counts(0, Some(5), Some(401))),
            ShapeVerdict::Fail,
            "a loop that wakes itself: no arm, no timer"
        );
    }

    #[test]
    fn a_bounded_wake_count_that_was_not_reported_is_not_a_pass() {
        assert_eq!(
            judge(&CLAUDE, counts(0, None, Some(200))),
            ShapeVerdict::Fail
        );
        assert_eq!(judge(&CLAUDE, counts(0, Some(5), None)), ShapeVerdict::Fail);
        let idle = SpinRow {
            scene: SpinScene::Idle,
            max_timer_wakes: None,
            ..CLAUDE
        };
        assert_eq!(judge(&idle, counts(0, None, Some(200))), ShapeVerdict::Pass);
        assert_eq!(judge(&idle, counts(0, None, None)), ShapeVerdict::Fail);
    }

    #[test]
    fn a_sleeping_pets_lane_fails_its_row_on_one_fire_or_none_reported() {
        let quiet = Counts {
            effect_fires: Some(0),
            ..counts(0, Some(20), Some(80))
        };
        assert_eq!(judge(&PET, quiet), ShapeVerdict::Pass);
        assert_eq!(
            judge(
                &PET,
                Counts {
                    effect_fires: Some(1),
                    ..quiet
                }
            ),
            ShapeVerdict::Fail,
            "a pet that fires in deep sleep, under every other bound"
        );
        assert_eq!(
            judge(
                &PET,
                Counts {
                    effect_fires: None,
                    ..quiet
                }
            ),
            ShapeVerdict::Fail
        );
        assert_eq!(
            judge(&CLAUDE, counts(0, Some(5), Some(200))),
            ShapeVerdict::Pass,
            "a row that bounds no lane passes whatever the lane did"
        );
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

    #[test]
    fn an_owners_fires_are_read_from_its_pair_and_an_absent_owner_fired_none() {
        let line = "OK timer_wakes=29 deadline_fires_by_owner=blink:19,cursor_effect:44,\
                    system_vitals:10 wakes_by_variant=none";
        assert_eq!(owner_fires(line, EFFECT_LANE), Some(44));
        assert_eq!(owner_fires(line, "blink"), Some(19));
        assert_eq!(
            owner_fires(line, "cursor"),
            Some(0),
            "a prefix is not an owner"
        );
        assert_eq!(
            owner_fires("OK deadline_fires_by_owner=none", EFFECT_LANE),
            Some(0)
        );
        assert_eq!(owner_fires("OK timer_wakes=0", EFFECT_LANE), None);
    }

    #[test]
    fn a_pet_take_decides_only_for_a_focused_windows_sleeping_pet_at_full_motion() {
        let ready = "OK 1\ntrail style=\"rainbow kitty pet\" resolved=rainbow-kitty focused=true \
                     motion=full motion_stage=full pet_active=true pet_action=sleep pet_pose=sleep_0";
        assert_eq!(pet_unready(ready), None);
        for (from, to, off) in [
            ("resolved=rainbow-kitty", "resolved=comet", "resolved=comet"),
            ("focused=true", "focused=false", "focused=false"),
            (
                "motion_stage=full",
                "motion_stage=reduced",
                "motion_stage=reduced",
            ),
            ("pet_active=true", "pet_active=false", "pet_active=false"),
            ("pet_action=sleep", "pet_action=loaf", "pet_action=loaf"),
            (" pet_action=sleep", "", "pet_action=?"),
        ] {
            assert_eq!(
                pet_unready(&ready.replacen(from, to, 1)).as_deref(),
                Some(off),
                "{from} -> {to:?}"
            );
        }
    }

    #[test]
    fn a_pet_takes_record_names_each_field_and_marks_one_the_reply_lacks() {
        let reply = "OK 1\ntrail style=\"rainbow kitty pet\" resolved=rainbow-kitty focused=true \
                     motion=full motion_stage=full pet_active=true pet_action=sleep";
        assert_eq!(
            pet_fields(reply),
            "resolved=rainbow-kitty focused=true motion_stage=full pet_active=true \
             pet_action=sleep pet_pose=?"
        );
    }
}
