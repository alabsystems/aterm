// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spin matrix: an idle instance must stay idle. A row bounds every wake its loop takes
//! (`event_wakes`: timer, wait-cancelled and poll wakes), not only its timers: a loop that wakes
//! itself as it parks counts wait-cancelled wakes and turns, never a timer wake.

use std::time::Duration;

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Ctl, Instance, Scratch, World, could_not_run, field, uint_field,
    unanswered,
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
    /// measured once the pet is in deep sleep ([`PET_DEEP_SLEEP`], read as `pet_deep` as the window
    /// opens). A pet asleep past its breath window takes no frame and offers no deadline, so the
    /// cursor-effect lane fires none while its window keeps the focus and takes no event. A window
    /// that loses the focus retires its pet, which fades back in over 0.30 s when the focus
    /// returns (about 18 frames at a 120 Hz panel's 16.7 ms effect interval), and a pointer over
    /// its pane heats the pet's gaze, so the scene's take decides only with no window event other
    /// than a redraw ([`os_events_unidle`]) and a pet that asks for no frame as the window opens.
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
    /// Deadline owners whose timer wakes in the window (`deadline_fires_by_owner`) the row
    /// bounds, each by its own count: the cursor-effect lane ([`EFFECT_LANE`]: the pet's frame
    /// train and offers, and the glow's and trail's). An owner the row names but the metrics
    /// leave unreported passes nothing.
    pub max_owner_fires: &'static [(&'static str, u64)],
    /// Whether the row bounds the stats deck's dial needle ([`NEEDLE_LANE`]) by the frames the
    /// app's readings owed it: its fires in the window at most the frames owed by the readings
    /// in the window (`needle_frames_owed`) and what the episode in flight as it opened had left
    /// to fire (`needle_episode_left`), and the wakes those fires take
    /// ([`WAKES_PER_NEEDLE_FRAME`] each)
    /// left out of [`Self::max_wakes`]. The dial reads the Mac's own CPU, so how far and how
    /// often it sweeps is the machine's, and that it asks no frame its sweeps do not owe is
    /// aterm's. A bound whose counts the metrics leave unreported passes nothing.
    pub needle_owed: bool,
    /// Every loop wake in the window (`event_wakes`).
    pub max_wakes: u64,
}

const SETTLE_QUIET_MS: &str = "3000";

/// The deadline owner the cursor effects (the pet, the glow, the trail) fire as.
pub const EFFECT_LANE: &str = "cursor_effect";

/// The deadline owner the stats deck's dial needle fires as while it sweeps to a reading.
pub const NEEDLE_LANE: &str = "stats_needle";

/// The loop wakes one needle frame takes: its deadline's timer wake and the `GpuSurfaceReady`
/// wake its present brings. Three pet takes on 2026-10-28 counted 19, 34 and 46 needle fires
/// beside 129, 160 and 184 wakes, as many `GpuSurfaceReady` wakes as frames: 91 or 92 wakes
/// besides the needle's in each.
pub const WAKES_PER_NEEDLE_FRAME: u64 = 2;

/// How long the pet scene waits on a quiet prompt before its window opens: past the pet's deep
/// sleep, which `kitty_pet.rs` begins `SLEEP_AFTER` (22 s) and `BREATH_WINDOW` (10 s) after the
/// caret last moved (a lab instance showed it about 34 s after the last key), with room.
pub const PET_DEEP_SLEEP: Duration = Duration::from_secs(40);

/// The cursor style the pet scene draws: the shipped default (`DEFAULT_CURSOR_TRAIL_STYLE`),
/// named so the scene keeps the pet if that default moves.
const PET_STYLE: &str = "rainbow kitty pet";

/// The `trail status` fields a pet take records: the style, the focus and motion its effects saw,
/// the pet's state, why it is in it, and its console events so far, which differ between the
/// window's edges where an event reached the pet between them.
const PET_FIELDS: [&str; 11] = [
    "resolved",
    "focused",
    "motion",
    "motion_stage",
    "shed",
    "pet_active",
    "pet_action",
    "pet_deep",
    "pet_pose",
    "pet_reason",
    "pet_event_seq",
];

/// What a pet take's `trail status` must read for its bound to decide anything: the pet drawn, in
/// a focused window at full motion, in deep sleep. A window without the focus moves its pet at
/// reduced motion and asks it for no frame, so a 0 read there passes any pet, and a pet still in
/// its breath window (one the focus reached late: its quiet gains at most 5 s without the focus)
/// draws its breaths and z's, as one still fading back in from a focus lost and regained draws
/// its fade and one a pointer warmed draws its gaze (`pet_deep` reads none of them).
const PET_READY: [(&str, &str); 6] = [
    ("resolved", "rainbow-kitty"),
    ("focused", "true"),
    ("motion_stage", "full"),
    ("pet_active", "true"),
    ("pet_action", "sleep"),
    ("pet_deep", "true"),
];

/// The first [`PET_READY`] field a `trail status` reply does not read as a pet take needs, as
/// `key=value` (`?` for one the reply lacks); `None` when the take can decide.
fn pet_unready(line: &str) -> Option<String> {
    PET_READY
        .iter()
        .find(|(key, want)| field(line, key) != Some(*want))
        .map(|(key, _)| format!("{key}={}", field(line, key).unwrap_or("?")))
}

/// The most window events other than a redraw (`ctl metrics os_window_events`) a windowed take may
/// see and still be idle: a focus or occlusion change is one or two, and a pointer over the window
/// or a key typed into it is many, which make the take a refusal, never a finding.
pub const MAX_OS_EVENTS: u64 = 30;

/// Why `scene`'s take cannot decide after `input` window events other than a redraw, or `None`
/// when it can: more than [`MAX_OS_EVENTS`] for a windowed scene, and any for
/// [`SpinScene::PetAsleep`], whose pet answers each (a focus lost and regained, two events, fades
/// it back in over 0.30 s, about 18 cursor-effect frames at a 16.7 ms effect interval; a pointer
/// over its pane heats its gaze; a resize can move its seat).
fn os_events_unidle(scene: SpinScene, input: u64) -> Option<&'static str> {
    if scene == SpinScene::PetAsleep {
        (input > 0).then_some("its pet may answer any of them")
    } else {
        (input > MAX_OS_EVENTS).then_some("more than an idle window takes")
    }
}

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
    /// The timer wakes in the window of each owner the row bounds
    /// ([`SpinRow::max_owner_fires`]), as the metrics read them.
    pub owner_fires: Vec<(&'static str, Option<u64>)>,
    /// The dial's needle in the window, as the metrics read it.
    pub needle: NeedleCounts,
    /// A pet take's [`PET_FIELDS`] as the window opened.
    pub pet_open: Option<String>,
    /// A pet take's [`PET_FIELDS`] at the end of the window.
    pub pet: Option<String>,
    /// A windowed take's window events other than a redraw (at most [`MAX_OS_EVENTS`]; none for
    /// the pet scene).
    pub os_events: Option<u64>,
    /// A windowed take's `os_window_events_by_kind`: what those events were (`focus:2`).
    pub os_event_kinds: Option<String>,
    /// The status reasons at the end of the window.
    pub reasons: Option<String>,
    pub verdict: ShapeVerdict,
    /// The session's replay checks at the end ([`super::instance::replay_checks`]).
    pub checks: String,
}

impl std::fmt::Display for SpinReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SPIN scene={:?} window={:?} past_deadline_arms={} max_arms={} stale_arm_heals={:?} \
             frames={:?} timer_wakes={:?} max_timer_wakes={:?} wakes={:?} max_wakes={} \
             owner_fires={:?} max_owner_fires={:?} needle_fires={:?} needle_sweeps={:?} \
             needle_frames_owed={:?} needle_episode_left={:?} needle_owed={} \
             wait_cancelled_wakes={:?} turns={:?} \
             wakes_by_variant={:?} fires_by_owner={:?} os_events={:?} os_event_kinds={:?} \
             pet_open={:?} pet={:?} \
             reasons={:?} \
             verdict={:?} {}",
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
            self.owner_fires,
            self.row.max_owner_fires,
            self.needle.fires,
            self.needle.sweeps,
            self.needle.owed,
            self.needle.carried,
            self.row.needle_owed,
            self.wait_cancelled_wakes,
            self.turns,
            self.wakes_by_variant,
            self.fires_by_owner,
            self.os_events,
            self.os_event_kinds,
            self.pet_open,
            self.pet,
            self.reasons,
            self.verdict,
            self.checks
        )
    }
}

/// The window's counters a row judges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts<'a> {
    pub arms: u64,
    pub timer_wakes: Option<u64>,
    /// The timer wakes of each owner the metrics reported ([`owner_fires`]).
    pub owner_fires: &'a [(&'static str, Option<u64>)],
    pub needle: NeedleCounts,
    pub wakes: Option<u64>,
}

/// The dial's needle in a window: its owner's fires ([`NEEDLE_LANE`]), the readings in the
/// window that set it sweeping (`needle_sweeps`) and the frames they owed (`needle_frames_owed`),
/// and what the episode in flight as the window opened had left to fire (`needle_episode_left`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NeedleCounts {
    pub fires: Option<u64>,
    pub sweeps: Option<u64>,
    pub owed: Option<u64>,
    pub carried: Option<u64>,
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

/// The row's bounds against the window's counters: past-deadline arms, timer wakes, each
/// bounded owner's fires ([`SpinRow::max_owner_fires`]), the needle's fires by the frames its
/// readings owed ([`SpinRow::needle_owed`]), and every loop wake (`wakes`) beyond the needle's
/// own, which an unreported count never passes.
#[must_use]
pub fn judge(row: &SpinRow, c: Counts<'_>) -> ShapeVerdict {
    let owners = row.max_owner_fires.iter().all(|&(owner, max)| {
        let fired = c
            .owner_fires
            .iter()
            .find(|(o, _)| *o == owner)
            .and_then(|&(_, n)| n);
        within(Some(max), fired)
    });
    // The needle's fires, once within what its readings owed; `None` past it or unreported.
    let needle = if row.needle_owed {
        match (c.needle.fires, c.needle.owed, c.needle.carried) {
            (Some(fires), Some(owed), Some(carried)) if fires <= owed.saturating_add(carried) => {
                Some(fires)
            }
            _ => None,
        }
    } else {
        Some(0)
    };
    let wakes = needle.and_then(|fires| {
        c.wakes
            .map(|n| n.saturating_sub(fires.saturating_mul(WAKES_PER_NEEDLE_FRAME)))
    });
    if c.arms <= row.max_arms
        && within(row.max_timer_wakes, c.timer_wakes)
        && owners
        && needle.is_some()
        && within(Some(row.max_wakes), wakes)
    {
        ShapeVerdict::Pass
    } else {
        ShapeVerdict::Fail
    }
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

/// `owner`'s timer wakes between the `ctl metrics` lines read as the window `opened` and as it
/// `ended`; `None` where either does not report them.
#[must_use]
pub fn fires_between(opened: &str, ended: &str, owner: &str) -> Option<u64> {
    Some(owner_fires(ended, owner)?.saturating_sub(owner_fires(opened, owner)?))
}

/// The needle's counts in the window, from the `ctl metrics` lines read as it `opened` and as it
/// `ended`: its fires, sweeps and owed frames between the two, and what the episode in flight had
/// left as it opened.
#[must_use]
pub fn needle_counts(opened: &str, ended: &str) -> NeedleCounts {
    let between =
        |key: &str| Some(uint_field(ended, key)?.saturating_sub(uint_field(opened, key)?));
    NeedleCounts {
        fires: fires_between(opened, ended, NEEDLE_LANE),
        sweeps: between("needle_sweeps"),
        owed: between("needle_frames_owed"),
        carried: uint_field(opened, "needle_episode_left"),
    }
}

/// The pet scene's [`PET_FIELDS`] as its window `edge` (opened, ended), refusing the take where
/// its `trail status` does not read a focused window's deeply sleeping pet at full motion
/// ([`PET_READY`]): a pet that is awake, still breathing or still fading in as the window opens
/// fires frames the scene does not measure.
fn pet_take(ctl: &Ctl, edge: &str) -> Result<String, CouldNotRun> {
    let trail = ctl.call(&["trail", "status"]);
    if !trail.ok() {
        return could_not_run(format!(
            "the pet scene's window did not hold the focus as its window {edge}, so its pet asked \
             for nothing (trail status: {} {})",
            trail.stdout.trim(),
            trail.stderr.trim()
        ));
    }
    let fields = pet_fields(&trail.stdout);
    if let Some(off) = pet_unready(&trail.stdout) {
        return could_not_run(format!(
            "the pet scene's window read {off} as its window {edge}, not a focused window's \
             deeply sleeping pet at full motion ({fields})"
        ));
    }
    Ok(fields)
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
/// moved the grid), the window took more than [`MAX_OS_EVENTS`] events other than
/// a redraw (a person's pointer or keys: not idle) or the pet scene's took any
/// ([`os_events_unidle`]), or the pet scene's window was not a focused window's
/// deeply sleeping pet at full motion ([`PET_READY`]) as it opened or as it ended.
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
    // Window events count from the reset, and the owners' fires from the metrics read after the
    // pet: what an event before the reset left the pet doing (its fade back in, its gaze's heat)
    // either reads here as a pet asking for frames or ended before the fires' count opened. The
    // read's own wake lands in the window, which its wake bound has room for.
    let pet_open = if row.scene == SpinScene::PetAsleep {
        Some(pet_take(ctl, "opened")?)
    } else {
        None
    };
    let opened = ctl.ok(&["metrics"])?;
    let before = field(&opened, "past_deadline_arms")
        .ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics does not report past_deadline_arms".into())
        })?
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
    let pet = if row.scene == SpinScene::PetAsleep {
        Some(pet_take(ctl, "ended")?)
    } else {
        None
    };
    let mut os_events = None;
    let mut os_event_kinds = None;
    if row.scene.windowed() {
        let input = uint_field(&metrics, "os_window_events").ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics does not report os_window_events".into())
        })?;
        let kinds = field(&metrics, "os_window_events_by_kind");
        os_events = Some(input);
        os_event_kinds = kinds.map(str::to_string);
        if let Some(why) = os_events_unidle(row.scene, input) {
            let events = if input == 1 { "event" } else { "events" };
            return could_not_run(format!(
                "the window took {input} {events} other than a redraw ({}) in {:?}: {why}",
                kinds.unwrap_or("kinds unreported"),
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
    let fired: Vec<(&'static str, Option<u64>)> = row
        .max_owner_fires
        .iter()
        .map(|&(owner, _)| (owner, fires_between(&opened, &metrics, owner)))
        .collect();
    if fired.iter().any(|(_, n)| n.is_none()) {
        return unanswered("ctl metrics does not report deadline_fires_by_owner");
    }
    let needle = needle_counts(&opened, &metrics);
    if row.needle_owed && (needle.owed.is_none() || needle.carried.is_none()) {
        return unanswered(
            "ctl metrics does not report needle_frames_owed and needle_episode_left",
        );
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
        pet_open,
        pet,
        os_events,
        os_event_kinds,
        reasons,
        verdict: judge(
            row,
            Counts {
                arms,
                timer_wakes,
                owner_fires: &fired,
                needle,
                wakes,
            },
        ),
        owner_fires: fired,
        needle,
        checks: super::instance::replay_checks(ctl),
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
        max_owner_fires: &[],
        needle_owed: false,
        max_wakes: 400,
    };

    const PET: SpinRow = SpinRow {
        scene: SpinScene::PetAsleep,
        max_timer_wakes: None,
        max_owner_fires: &[(EFFECT_LANE, 0)],
        needle_owed: true,
        max_wakes: 200,
        ..CLAUDE
    };

    const fn counts(arms: u64, timer_wakes: Option<u64>, wakes: Option<u64>) -> Counts<'static> {
        Counts {
            arms,
            timer_wakes,
            owner_fires: &[],
            needle: NeedleCounts {
                fires: Some(0),
                sweeps: Some(0),
                owed: Some(0),
                carried: Some(0),
            },
            wakes,
        }
    }

    /// A pet take whose needle fired `fires` times against the frames its window's readings owed
    /// and what the episode in flight as it opened had left, among `wakes` loop wakes.
    const fn needle(fires: u64, (owed, carried): (u64, u64), wakes: u64) -> Counts<'static> {
        Counts {
            owner_fires: &[(EFFECT_LANE, Some(0))],
            needle: NeedleCounts {
                fires: Some(fires),
                sweeps: None,
                owed: Some(owed),
                carried: Some(carried),
            },
            ..counts(0, None, Some(wakes))
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
        let calm = [(EFFECT_LANE, Some(0))];
        let quiet = Counts {
            owner_fires: &calm,
            ..counts(0, Some(20), Some(80))
        };
        assert_eq!(judge(&PET, quiet), ShapeVerdict::Pass);
        let pet = [(EFFECT_LANE, Some(1))];
        assert_eq!(
            judge(
                &PET,
                Counts {
                    owner_fires: &pet,
                    ..quiet
                }
            ),
            ShapeVerdict::Fail,
            "a pet that fires in deep sleep, under every other bound"
        );
        let unread = [(EFFECT_LANE, None)];
        assert_eq!(
            judge(
                &PET,
                Counts {
                    owner_fires: &unread,
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

    /// The pet row bounds the dial's needle by the frames its readings owed, which the Mac's
    /// load sets: a busy take passes where its readings owed its fires, as does an episode in
    /// flight as the window opened, and the wakes the needle's fires take are its own. A needle
    /// that fires one frame more than its readings owed fails under every other bound, as a wake
    /// beyond the needle's own does, and as an unreported count does; the sweeps alone are a
    /// record, not a bound.
    #[test]
    fn the_pet_row_bounds_the_needle_by_the_frames_its_readings_owed() {
        for (what, c, verdict) in [
            ("a quiet take", needle(0, (0, 0), 92), ShapeVerdict::Pass),
            (
                "a quiet take's drift",
                needle(3, (3, 0), 98),
                ShapeVerdict::Pass,
            ),
            (
                "a busy Mac's take",
                needle(46, (51, 0), 184),
                ShapeVerdict::Pass,
            ),
            (
                "an episode in flight",
                needle(46, (30, 16), 184),
                ShapeVerdict::Pass,
            ),
            (
                "one frame past",
                needle(47, (30, 16), 186),
                ShapeVerdict::Fail,
            ),
            (
                "a needle that owes nothing",
                needle(1, (0, 0), 94),
                ShapeVerdict::Fail,
            ),
            (
                "its own two wakes a frame",
                needle(20, (20, 0), 240),
                ShapeVerdict::Pass,
            ),
            (
                "one wake beyond them",
                needle(20, (20, 0), 241),
                ShapeVerdict::Fail,
            ),
        ] {
            assert_eq!(judge(&PET, c), verdict, "{what}");
        }
        let busy = needle(46, (51, 0), 184);
        for unread in [
            NeedleCounts {
                fires: None,
                ..busy.needle
            },
            NeedleCounts {
                owed: None,
                ..busy.needle
            },
            NeedleCounts {
                carried: None,
                ..busy.needle
            },
        ] {
            assert_eq!(
                judge(
                    &PET,
                    Counts {
                        needle: unread,
                        ..busy
                    }
                ),
                ShapeVerdict::Fail,
                "{unread:?}"
            );
        }
        assert_eq!(
            judge(
                &CLAUDE,
                Counts {
                    needle: NeedleCounts {
                        fires: Some(100),
                        owed: Some(100),
                        ..busy.needle
                    },
                    ..counts(0, Some(5), Some(401))
                }
            ),
            ShapeVerdict::Fail,
            "a row that bounds no needle leaves none of its wakes out"
        );
        let opened = "OK past_deadline_arms=3 deadline_fires_by_owner=stats_needle:2 \
                      needle_sweeps=1 needle_frames_owed=8 needle_episode_left=6";
        let ended = "OK past_deadline_arms=3 deadline_fires_by_owner=stats_live:30,stats_needle:11 \
                     needle_sweeps=2 needle_frames_owed=20 needle_episode_left=0";
        assert_eq!(
            needle_counts(opened, ended),
            NeedleCounts {
                fires: Some(9),
                sweeps: Some(1),
                owed: Some(12),
                carried: Some(6),
            },
            "what was left as the window opened, the rest between its edges"
        );
        assert_eq!(
            needle_counts("OK", ended),
            NeedleCounts::default(),
            "an opening read that reports nothing leaves every count unreported"
        );
        assert_eq!(
            fires_between("OK deadline_fires_by_owner=none", ended, EFFECT_LANE),
            Some(0)
        );
        let line = "OK timer_wakes=46 deadline_fires_by_owner=stats_live:30,stats_needle:4,\
                    system_vitals:10 wakes_by_variant=none";
        assert_eq!(owner_fires(line, NEEDLE_LANE), Some(4));
        assert_eq!(owner_fires(line, EFFECT_LANE), Some(0));
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
                     motion=full motion_stage=full pet_active=true pet_action=sleep \
                     pet_deep=true pet_pose=sleep_0";
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
            ("pet_deep=true", "pet_deep=false", "pet_deep=false"),
        ] {
            assert_eq!(
                pet_unready(&ready.replacen(from, to, 1)).as_deref(),
                Some(off),
                "{from} -> {to:?}"
            );
        }
    }

    #[test]
    fn a_pet_take_decides_only_with_no_window_event_its_pet_would_answer() {
        assert!(
            os_events_unidle(SpinScene::PetAsleep, 2).is_some(),
            "a focus lost and regained fades the pet back in"
        );
        assert!(
            os_events_unidle(SpinScene::PetAsleep, 1).is_some(),
            "one event is any"
        );
        assert_eq!(os_events_unidle(SpinScene::PetAsleep, 0), None);
        // CONTROL: the bare windowed scene keeps its allowance.
        assert_eq!(os_events_unidle(SpinScene::Windowed, 2), None);
        assert!(os_events_unidle(SpinScene::Windowed, MAX_OS_EVENTS + 1).is_some());
    }

    #[test]
    fn a_pet_takes_record_names_each_field_and_marks_one_the_reply_lacks() {
        let reply = "OK 1\ntrail style=\"rainbow kitty pet\" resolved=rainbow-kitty focused=true \
                     motion=full motion_stage=full pet_active=true pet_action=sleep";
        assert_eq!(
            pet_fields(reply),
            "resolved=rainbow-kitty focused=true motion=full motion_stage=full shed=? \
             pet_active=true pet_action=sleep pet_deep=? pet_pose=? pet_reason=? pet_event_seq=?"
        );
    }
}
