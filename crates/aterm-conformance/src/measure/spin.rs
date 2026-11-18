// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spin matrix: an idle instance must stay idle. A row bounds every wake its loop takes
//! (`event_wakes`: timer, wait-cancelled and poll wakes), not only its timers: a loop that wakes
//! itself as it parks counts wait-cancelled wakes and turns, never a timer wake.

use std::time::Duration;

use super::Rig;
use super::instance::{
    Budget, CouldNotRun, Ctl, Instance, Scratch, World, could_not_run, field, recording_line,
    require_recording, uint_field, unanswered,
};
use super::starvation::{Disposition, ShapeVerdict};

#[path = "spin_unattended.rs"]
pub mod unattended;

/// What sits on the instance while it is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinScene {
    /// The steady repainter.
    ClaudeShaped,
    /// A bare `/bin/sh` prompt: the floor.
    Idle,
    /// A recording `/bin/sh` prompt frozen in an open Rewind view.
    Rewound,
    /// [`Self::Rewound`] in a window under `motion = "reduced"`, as [`Self::Windowed`]: the GPU
    /// present of a rewound pane (its band, border and past screen) on AppKit's loop, which runs
    /// the cursor blink and the proxy deadlines a headless instance never arms. A focused
    /// window blinks a live caret ([`BLINK_LANE`]); over a rewound pane it must not, so the take
    /// raises its window and waits for the focus before it settles, and decides only where the
    /// window is focused ([`FOCUSED`]) as its measured window opens and as it ends, and was drawn
    /// by the GPU renderer.
    /// The reduced motion is also what leaves the caret to the blink clock: at full motion the
    /// shipped rainbow cursor owns a blinking block's caret, and the blink lane fires for no
    /// pane, live or rewound.
    WindowedRewound,
    /// A bare `/bin/sh` prompt in a window: AppKit's loop and its proxies, which a headless
    /// instance never runs. Under `motion = "reduced"`: a focused window's motion (the Deck's live
    /// edge, the pet) wakes it many times a second, and the rig does not decide whether the window
    /// it opens holds the focus.
    Windowed,
    /// [`Self::Windowed`] at full motion under the shipped cursor, the rainbow beside the cat,
    /// measured once the pet is in deep sleep (read as `pet_deep` as the window
    /// opens). A pet asleep past its breath window takes no frame and offers no deadline, so the
    /// cursor-effect lane fires none while its window keeps the focus and takes no event. A window
    /// that loses the focus retires its pet, which fades back in over 0.30 s when the focus
    /// returns (about 18 frames at a 120 Hz panel's 16.7 ms effect interval), and a pointer over
    /// its pane heats the pet's gaze, so the scene's take decides only with no window event other
    /// than a redraw ([`os_events_unidle`]) and a pet that asks for no frame as the window opens.
    /// The window's rest is off (`idle_rest_secs = 0`), so the take measures an awake window's
    /// deck; a take whose window rests anyway is a refusal ([`rest_unready`]).
    PetAsleep,
    /// [`Self::PetAsleep`] with the rest on at its measured candidate (`idle_rest_secs = 30`; off
    /// unless set): the window must have rested (ATERM-KI-1027-4), so its deck
    /// draws still on background readings. The take decides only once the window reads
    /// `resting=true` as it opens and as it ends, and, since any window event would wake it, only
    /// with no window event other than a redraw.
    Resting,
    /// A `/bin/sh` prompt in a window at full motion under the pet, raised, then minimised
    /// (`invoke Minimize`) while a counter it was typed prints a line once a second
    /// ([`HIDDEN_COUNTER`]), measured once the window reads `sight=off-glass` (`trail status`): a
    /// window on no glass composes and presents nothing (`docs/ATERM_DESIGN.md` §7.4), however
    /// much its pane prints, and its clocks plan no frame. The take decides only on a window that
    /// reads off-glass as it opens and as it ends ([`hidden_unready`]), with no window event
    /// other than a redraw between (a show or a focus would be one). A build whose `trail status`
    /// reports no `sight=` ([`hidden_unanswered`]), or whose metrics report no
    /// `redraw_hidden_parked`, leaves it unanswered.
    Hidden,
    /// A bare `/bin/sh` prompt in a window at the shipped motion, rest and effect defaults (its
    /// head writes no motion, rest or effect line: `motion = "auto"`, the window never rests,
    /// the rainbow cursor and the cat), in the scratch world every take runs in (its bell silent
    /// and its automatic lanes off, [`World::prepare`]), raised, then left untouched until its
    /// attention lapses ([`unattended::ATTENTION`] after the raise, whose focus is a touch),
    /// measured once `trail status` reads it focused, unattended, on glass and with its pet in
    /// deep sleep or not drawn ([`unattended::unready`]) and the readings have settled on their
    /// background cadence ([`unattended::SETTLE`]), as it opens and as it ends, with no window
    /// event between. Its motion stage and shed are recorded, never required: the Mac's Reduce
    /// Motion and the load shed may move them. Its decision is [`unattended::decide`]'s: counts,
    /// never time.
    Unattended,
}

impl SpinScene {
    /// Whether the scene opens a window.
    #[must_use]
    pub const fn windowed(self) -> bool {
        matches!(
            self,
            Self::Windowed
                | Self::PetAsleep
                | Self::Resting
                | Self::WindowedRewound
                | Self::Hidden
                | Self::Unattended
        )
    }

    /// Whether the scene measures a focused window's sleeping pet ([`PET_READY`]).
    const fn pet(self) -> bool {
        matches!(self, Self::PetAsleep | Self::Resting)
    }

    /// Whether the scene opens a Rewind view on its pane, which needs a recording pane.
    #[must_use]
    pub const fn rewound(self) -> bool {
        matches!(self, Self::Rewound | Self::WindowedRewound)
    }

    /// The scene's own config lines, before the row's recording line ([`head`]).
    #[must_use]
    pub fn config(self) -> String {
        match self {
            Self::Windowed | Self::WindowedRewound => "motion = \"reduced\"".to_string(),
            // `full`, not the shipped `auto`: what `auto` resolves to on a Mac whose Reduce
            // Motion is off, so the take's pet animates whatever this Mac's accessibility setting
            // says.
            // The awake control turns the window's rest off, so the deck it measures is a
            // focused window's whatever the default; the resting scene turns it on at 30 s.
            Self::PetAsleep => {
                format!(
                    "motion = \"full\"\ncursor_trail_style = \"{PET_STYLE}\"\ncursor_pet = \"cat\"\nidle_rest_secs = 0"
                )
            }
            Self::Resting => {
                format!(
                    "motion = \"full\"\ncursor_trail_style = \"{PET_STYLE}\"\ncursor_pet = \"cat\"\nidle_rest_secs = 30"
                )
            }
            // The pet the typed counter wakes would ask frames of a window that cannot show them.
            Self::Hidden => {
                format!(
                    "motion = \"full\"\ncursor_trail_style = \"{PET_STYLE}\"\ncursor_pet = \"cat\""
                )
            }
            Self::ClaudeShaped | Self::Idle | Self::Rewound => {
                "# effect settings: the shipped defaults".to_string()
            }
            Self::Unattended => {
                "# motion, rest and effect settings: the shipped defaults".to_string()
            }
        }
    }
}

/// A row's config head: its scene's lines, then its recording ([`recording_line`]), set either
/// way so the row records or not whatever the shipped default is.
#[must_use]
pub fn head(row: &SpinRow) -> String {
    format!("{}\n{}", row.scene.config(), recording_line(row.recording))
}

/// One spin row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpinRow {
    pub scene: SpinScene,
    /// Whether the instance records (`temporal_recording`), written either way: on is the
    /// shipped default's arm, and off the arm of a person who turns Settings > Terminal > Rewind
    /// off. A take whose `temporal status` reads the other arm decides nothing
    /// ([`require_recording`]).
    pub recording: bool,
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
    /// The frames the window presented (`frames`), where the row bounds them; `None` bounds
    /// nothing.
    pub max_frames: Option<u64>,
    /// The fewest redraws a hidden window parks in the window (`redraw_hidden_parked`), where the
    /// row asks for them: the output its pane printed reached the window and was held, so a
    /// take whose output never came, or a build that parks nothing, passes no frame bound by
    /// drawing nothing. `None` asks for none.
    pub min_hidden_parked: Option<u64>,
}

impl SpinRow {
    /// The combinations that could not measure what they claim: a Rewind view opens only on a
    /// recording pane.
    /// # Errors
    /// The row contradicts itself.
    pub fn validate(&self) -> Result<(), CouldNotRun> {
        if self.scene.rewound() && !self.recording {
            return could_not_run(format!(
                "the {:?} scene opens a Rewind view, which needs a recording pane",
                self.scene
            ));
        }
        Ok(())
    }
}

const SETTLE_QUIET_MS: &str = "3000";

/// The deadline owner the cursor effects (the pet, the glow, the trail) fire as.
pub const EFFECT_LANE: &str = "cursor_effect";

/// The deadline owner the stats deck's dial needle fires as while it sweeps to a reading: its
/// deck clock's (`aterm-gui`'s `stats_clock.rs`), the synthetic grid's ticks and a display
/// link's resumes.
pub const NEEDLE_LANE: &str = "deck_clock";

/// The deadline owner the cursor blink fires as: a focused window's live caret only.
pub const BLINK_LANE: &str = "blink";

/// What a windowed Rewound take's `trail status` must read for its blink bound to decide
/// anything: a window without the focus blinks no caret, so a 0 read there passes any build.
pub const FOCUSED: (&str, &str) = ("focused", "true");

/// The deadline owner the stats deck's live edge (its glide, slide, beat and steam) fires as.
pub const LIVE_LANE: &str = "stats_live";

/// The loop wakes one needle frame takes: its deadline's timer wake. Its present brings none: the
/// drawable worker posts `GpuSurfaceReady` only for a frame that found no drawable and waits
/// (`aterm-gpu`'s `metal/acquire_worker.rs`). Three pet takes on 2026-10-28, on a build whose
/// worker posted that wake after every acquisition, counted 19, 34 and 46 needle fires beside
/// 129, 160 and 184 wakes, as many `GpuSurfaceReady` wakes as frames: 91 or 92 wakes besides
/// the needle's two a fire in each. Those takes also predate the deck clock: a window on its
/// display link fires [`NEEDLE_LANE`] only to resume the paused link, one timer wake, and draws
/// its frames on the link's ticks, window events that [`SpinRow::max_wakes`] counts.
pub const WAKES_PER_NEEDLE_FRAME: u64 = 1;

/// The pet rows' existing total deadline, including their readiness wait and observation.
const PET_BUDGET_SECS: u64 = 130;

/// The cursor style the pet scene draws: the shipped default (`DEFAULT_CURSOR_TRAIL_STYLE`),
/// named so the scene keeps the pet if that default moves.
const PET_STYLE: &str = "rainbow";

/// The `trail status` fields a pet take records: the style, the focus and motion its effects saw,
/// the pet's state, why it is in it, its console events so far, which differ between the
/// window's edges where an event reached the pet between them, and whether the window rests.
const PET_FIELDS: [&str; 12] = [
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
    "resting",
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

/// Whether a pet take's `trail status` reads the window's rest as `scene` needs it, as
/// `resting=value` (`?` for a reply without it) when it does not: a [`SpinScene::Resting`] take
/// decides only on a window that rests, and a [`SpinScene::PetAsleep`] take only on one awake, so
/// a rest its `idle_rest_secs = 0` failed to turn off is a refusal, never a pass on a still deck.
/// A reply without the field (a build from before the rest) is awake.
fn rest_unready(scene: SpinScene, line: &str) -> Option<String> {
    let resting = field(line, "resting");
    let ready = match scene {
        SpinScene::Resting => resting == Some("true"),
        SpinScene::PetAsleep => resting != Some("true"),
        SpinScene::ClaudeShaped
        | SpinScene::Idle
        | SpinScene::Rewound
        | SpinScene::Windowed
        | SpinScene::WindowedRewound
        | SpinScene::Hidden
        | SpinScene::Unattended => true,
    };
    (!ready).then(|| format!("resting={}", resting.unwrap_or("?")))
}

/// What the hidden scene types into its prompt before the window is minimised: a line once a
/// second for longer than any take, so the pane prints all through the measured window.
pub const HIDDEN_COUNTER: &str =
    "i=0; while [ $i -lt 600 ]; do i=$((i+1)); echo tick $i; sleep 1; done";

/// Why a build cannot answer the hidden scene, from its `trail status` reply before the window is
/// minimised: one from before the field reports no `sight=`, so its window could never read off
/// every glass, and the row is unanswered rather than refused.
fn hidden_unanswered(line: &str) -> Option<&'static str> {
    field(line, "sight")
        .is_none()
        .then_some("ctl trail status does not report sight")
}

/// Where `trail status` says a hidden take's window can be seen, as `sight=value` (`?` for a reply
/// without it) when it is not off every glass; `None` when the take can decide.
fn hidden_unready(line: &str) -> Option<String> {
    let sight = field(line, "sight");
    (sight != Some("off-glass")).then(|| format!("sight={}", sight.unwrap_or("?")))
}

/// How often a windowed take reads the instance's footprint ([`super::usage`]).
pub const SAMPLE_EVERY: Duration = Duration::from_millis(50);

/// How far above a take's least footprint a sample may sit and still count as at the floor: the
/// GPU driver's render-pass block is about 179 MB, so a sample within this has it released
/// (`docs/measured/idle-memory-window-2026-10-27.md`).
pub const FLOOR_BAND: u64 = 32 << 20;

/// A windowed take's footprint over its window, sampled every [`SAMPLE_EVERY`], and what the
/// instance used in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sampled {
    pub samples: usize,
    /// The samples' median, least and greatest footprint, bytes.
    pub p50: u64,
    pub min: u64,
    pub max: u64,
    /// The samples within [`FLOOR_BAND`] of the least.
    pub at_floor: usize,
    /// The CPU time the instance took between the first sample and the last, ns.
    pub cpu_ns: u64,
    /// The wall time `cpu_ns` was taken in: from just after the first sample to just after the
    /// last, ns.
    pub span_ns: u64,
}

impl Sampled {
    /// The summary of `footprints` (`None` for none), with the CPU time the instance took
    /// (`cpu_ns`) in the wall time the samples spanned (`span_ns`).
    #[must_use]
    pub fn of(footprints: &[u64], cpu_ns: u64, span_ns: u64) -> Option<Self> {
        let mut sorted = footprints.to_vec();
        sorted.sort_unstable();
        let min = *sorted.first()?;
        let max = *sorted.last()?;
        Some(Self {
            samples: sorted.len(),
            p50: sorted[sorted.len() / 2],
            min,
            max,
            at_floor: sorted
                .iter()
                .filter(|&&f| f <= min.saturating_add(FLOOR_BAND))
                .count(),
            cpu_ns,
            span_ns,
        })
    }

    /// The instance's CPU time per second of wall time over the samples, ms: [`Self::cpu_ns`]
    /// over [`Self::span_ns`]; 0 for samples that spanned no time. A report, never a bound.
    #[must_use]
    pub fn cpu_ms_per_s(&self) -> f64 {
        if self.span_ns == 0 {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a CPU rate for reading, from counts far under 2^52"
        )]
        let rate = self.cpu_ns as f64 * 1e3 / self.span_ns as f64;
        rate
    }

    /// The share of the samples at the floor ([`Self::at_floor`]), 0 to 1.
    #[must_use]
    pub fn floor_share(&self) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a share of a few hundred samples"
        )]
        let share = self.at_floor as f64 / self.samples.max(1) as f64;
        share
    }
}

impl std::fmt::Display for Sampled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mib = |b: u64| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a footprint in MiB, for reading"
            )]
            let m = b as f64 / f64::from(1u32 << 20);
            m
        };
        write!(
            f,
            "samples={} footprint_p50_mib={:.1} footprint_min_mib={:.1} footprint_max_mib={:.1} \
             floor_share={:.2} cpu_ns={} cpu_ms_per_s={:.3}",
            self.samples,
            mib(self.p50),
            mib(self.min),
            mib(self.max),
            self.floor_share(),
            self.cpu_ns,
            self.cpu_ms_per_s()
        )
    }
}

/// Watches `pid` for `window`, reading its footprint every [`SAMPLE_EVERY`].
#[cfg(target_os = "macos")]
fn sample(pid: u32, window: Duration) -> Result<Option<Sampled>, CouldNotRun> {
    let start = std::time::Instant::now();
    let first = super::usage(pid)?;
    let first_at = std::time::Instant::now();
    let mut footprints = vec![first.footprint];
    let mut last = first;
    loop {
        let ran = start.elapsed();
        if ran >= window {
            break;
        }
        std::thread::sleep(SAMPLE_EVERY.min(window - ran));
        last = super::usage(pid)?;
        footprints.push(last.footprint);
    }
    let span = u64::try_from(first_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Ok(Sampled::of(
        &footprints,
        last.cpu_ns.saturating_sub(first.cpu_ns),
        span,
    ))
}

/// Where no footprint is named, the window passes unsampled.
#[cfg(not(target_os = "macos"))]
fn sample(_pid: u32, window: Duration) -> Result<Option<Sampled>, CouldNotRun> {
    std::thread::sleep(window);
    Ok(None)
}

/// The most window events other than a redraw (`ctl metrics os_window_events`) a windowed take may
/// see and still be idle: a focus or occlusion change is one or two, and a pointer over the window
/// or a key typed into it is many, which make the take a refusal, never a finding.
pub const MAX_OS_EVENTS: u64 = 30;

/// Why `scene`'s take cannot decide after `input` window events other than a redraw, or `None`
/// when it can: more than [`MAX_OS_EVENTS`] for a windowed scene, and any for a pet scene, whose
/// pet answers each (a focus lost and regained, two events, fades it back in over 0.30 s, about
/// 18 cursor-effect frames at a 16.7 ms effect interval; a pointer over its pane heats its gaze; a
/// resize can move its seat) and whose window each but a focus leaving wakes from rest. An
/// unattended take never asks: [`unattended::decide`] refuses it at any event first.
fn os_events_unidle(scene: SpinScene, input: u64) -> Option<&'static str> {
    if scene.pet() {
        (input > 0).then_some("its pet may answer any of them")
    } else if scene == SpinScene::Hidden {
        (input > 0).then_some("a minimised window takes none until it is shown")
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
    /// A windowed take's footprint and CPU time over its window ([`Sampled`]).
    pub sampled: Option<Sampled>,
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
    /// The redraws a hidden window parked in the window (`redraw_hidden_parked`).
    pub hidden_parked: Option<u64>,
    /// Display-clock counters over the observation window, for wake diagnosis.
    pub deck_clock: Vec<(&'static str, Option<u64>)>,
    /// A pet take's [`PET_FIELDS`] as the window opened.
    pub pet_open: Option<String>,
    /// A pet take's [`PET_FIELDS`] at the end of the window.
    pub pet: Option<String>,
    /// A windowed Rewound take's renderer (`backend=`), read once its window ended focused.
    pub backend: Option<String>,
    /// A windowed take's window events other than a redraw (at most [`MAX_OS_EVENTS`]; none for
    /// the pet scene).
    pub os_events: Option<u64>,
    /// A windowed take's `os_window_events_by_kind`: what those events were (`focus:2`).
    pub os_event_kinds: Option<String>,
    /// The status reasons at the end of the window.
    pub reasons: Option<String>,
    /// The row's verdict: [`judge`]'s, a pass or a fail, for every scene but the unattended
    /// one, whose [`unattended::decide`] may also leave it undecided
    /// ([`Disposition::Unproved`]).
    pub verdict: Disposition,
    /// An unattended take's decision: its counts, bounds and edges, and why it failed or was
    /// left undecided.
    pub unattended: Option<unattended::UnattendedDecision>,
    /// The session's replay checks at the end ([`super::instance::replay_checks`]).
    pub checks: String,
}

impl std::fmt::Display for SpinReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SPIN scene={:?} recording={} window={:?} past_deadline_arms={} max_arms={} \
             stale_arm_heals={:?} \
             frames={:?} timer_wakes={:?} max_timer_wakes={:?} wakes={:?} max_wakes={} \
             owner_fires={:?} max_owner_fires={:?} needle_fires={:?} needle_sweeps={:?} \
             needle_frames_owed={:?} needle_episode_left={:?} needle_owed={} deck_clock={:?} \
             wait_cancelled_wakes={:?} turns={:?} \
             wakes_by_variant={:?} fires_by_owner={:?} os_events={:?} os_event_kinds={:?} \
             pet_open={:?} pet={:?} backend={:?} \
             reasons={:?} max_frames={:?} hidden_parked={:?} min_hidden_parked={:?} {}{} \
             verdict={:?} {}",
            self.row.scene,
            self.row.recording,
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
            self.deck_clock,
            self.wait_cancelled_wakes,
            self.turns,
            self.wakes_by_variant,
            self.fires_by_owner,
            self.os_events,
            self.os_event_kinds,
            self.pet_open,
            self.pet,
            self.backend,
            self.reasons,
            self.row.max_frames,
            self.hidden_parked,
            self.row.min_hidden_parked,
            self.sampled
                .map_or_else(|| "sampled=none".to_string(), |s| s.to_string()),
            self.unattended
                .as_ref()
                .map_or_else(String::new, |d| format!(" {d}")),
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
    /// The frames presented in the window (`frames`).
    pub frames: Option<u64>,
    /// The redraws a hidden window parked in the window (`redraw_hidden_parked`).
    pub hidden_parked: Option<u64>,
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
/// readings owed ([`SpinRow::needle_owed`]), every loop wake (`wakes`) beyond the needle's own
/// and, where the row bounds them, the frames ([`SpinRow::max_frames`]) and a hidden window's
/// parked redraws from below ([`SpinRow::min_hidden_parked`]); an unreported count never passes
/// its bound.
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
        && within(row.max_frames, c.frames)
        && row
            .min_hidden_parked
            .is_none_or(|min| c.hidden_parked.is_some_and(|n| n >= min))
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

/// A pet scene's [`PET_FIELDS`] as its window `edge` (opened, ended), refusing the take where
/// its `trail status` does not read a focused window's deeply sleeping pet at full motion
/// ([`PET_READY`]), resting as `scene` needs ([`rest_unready`]): a pet that is awake, still
/// breathing or still fading in as the window opens fires frames the scene does not measure, and
/// a window that rests or not against its scene measures the other scene's deck.
fn pet_take(ctl: &Ctl, scene: SpinScene, edge: &str) -> Result<String, CouldNotRun> {
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
    if let Some(off) = rest_unready(scene, &trail.stdout) {
        return could_not_run(format!(
            "the {scene:?} scene's window read {off} as its window {edge}, not resting as the \
             scene needs ({fields})"
        ));
    }
    Ok(fields)
}

/// How long the hidden scene waits after its window reads off-glass before its counters open:
/// the minimise animation and the focus it moves end well inside it.
const HIDDEN_SETTLE: Duration = Duration::from_secs(2);

/// Refuses the hidden scene's take where `trail status` does not read its window off every glass
/// as its window `edge` (opened, ended) ([`hidden_unready`]).
fn hidden_take(ctl: &Ctl, edge: &str) -> Result<(), CouldNotRun> {
    let trail = ctl.call(&["trail", "status"]);
    if !trail.ok() {
        return could_not_run(format!(
            "the hidden scene's window gave no trail status as its window {edge} ({} {})",
            trail.stdout.trim(),
            trail.stderr.trim()
        ));
    }
    match hidden_unready(&trail.stdout) {
        None => Ok(()),
        Some(off) => could_not_run(format!(
            "the hidden scene's window read {off} as its window {edge}, not minimised"
        )),
    }
}

/// Wait for the same state the observation's two edges require. Probe traffic precedes the
/// metrics reset. A failed read ends this attempt; a missing state never becomes a pass.
fn await_pet_ready(
    budget: Budget,
    scene: SpinScene,
    mut read: impl FnMut() -> Result<String, CouldNotRun>,
) -> Result<(), CouldNotRun> {
    let mut last = String::new();
    budget
        .await_event(
            "the focused pet's deep sleep and the scene's rest state",
            || {
                let line = match read() {
                    Ok(line) => line,
                    Err(error) => return Some(Err(error)),
                };
                let ready = pet_unready(&line).is_none() && rest_unready(scene, &line).is_none();
                last = pet_fields(&line);
                ready.then_some(Ok(()))
            },
        )
        .map_err(|error| error.map(|why| format!("{why}; last trail state: {last}")))?
}

/// Raise the sole test session's native window and wait for its focus: a pet scene's readiness
/// wait starts from it, the unattended scene's attention lapses from it, and the hidden scene
/// minimises the front window. Sending a PTY focus report would not establish this OS
/// precondition.
fn prepare_focus(ctl: &Ctl) -> Result<(), CouldNotRun> {
    let sessions = ctl.call(&["ls"]);
    if !sessions.ok() {
        return could_not_run("cannot list the measurement's session");
    }
    let sid = sole_session(&sessions.stdout)
        .ok_or_else(|| CouldNotRun::Refused("the measurement needs exactly one session".into()))?;
    ctl.ok(&["raise", sid])?;
    ctl.await_event("the measurement's native window to gain focus", || {
        let reply = ctl.call(&["trail", "status"]);
        (reply.ok() && field(&reply.stdout, "focused") == Some("true")).then_some(())
    })
}

/// The unattended take's whole-run limit: the ordinary take's [`BUDGET_SECS`], with its
/// attention lapse, its readiness wait's guard ([`UNATTENDED_GUARD`]), its settle
/// ([`unattended::SETTLE`]) and its longer `window` added.
fn unattended_budget_secs(window: Duration) -> u64 {
    BUDGET_SECS
        .saturating_add(unattended::ATTENTION.as_secs())
        .saturating_add(UNATTENDED_GUARD.as_secs())
        .saturating_add(unattended::SETTLE.as_secs())
        .saturating_add(window.as_secs())
}

/// How long the unattended take waits for its window's state on an instance that keeps working
/// without it: `aterm_spec::wait::WORKING_GUARD`'s length (aterm-spec is no library dependency
/// of this crate), a guard on a subject that works forever, not a latency budget. The take's
/// budget holds it ([`unattended_budget_secs`]), so a launch and settle that a loaded Mac
/// stretches leave the wait the rest of it.
const UNATTENDED_GUARD: Duration = Duration::from_secs(15 * 60);

/// How often the unattended take reads `trail status` while it waits for its window's state: each
/// read starts a `ctl` process, and the state it waits for comes at a lapse and a pet's deep sleep,
/// seconds apart, so a read a second is plenty.
const UNATTENDED_POLL: Duration = Duration::from_secs(1);

/// Wait, from the raise that touched the window last (`focused_at`), until its attention has
/// lapsed ([`unattended::ATTENTION`]) without a read, then read `trail status` until it reads
/// the unattended scene's state ([`unattended::unready`]), for as long as the instance (`pid`)
/// works ([`Budget::await_while_working`]): the wait is refused once the instance made no CPU or
/// I/O progress for a `MUST_HAPPEN`. Each read costs the instance the CPU of its answer, and its
/// readings run on their own, so an instance that still answers or reads is read as working, and
/// what ends the wait on one that keeps working without the state is the take's budget, which
/// holds [`UNATTENDED_GUARD`] for it. A build whose reply lacks the scene's fields leaves the
/// take unanswered ([`unattended::unanswered`]).
fn await_unattended(
    ctl: &Ctl,
    pid: u32,
    focused_at: std::time::Instant,
) -> Result<(), CouldNotRun> {
    let first = ctl.call(&["trail", "status"]);
    if let Some(why) = first
        .ok()
        .then(|| unattended::unanswered(&first.stdout))
        .flatten()
    {
        return unanswered(why);
    }
    std::thread::sleep(unattended::ATTENTION.saturating_sub(focused_at.elapsed()));
    let mut last = String::new();
    ctl.budget()
        .await_while_working(
            pid,
            "the window's unattended state",
            UNATTENDED_POLL,
            || {
                let reply = ctl.call(&["trail", "status"]);
                if !reply.ok() {
                    last = format!("{} {}", reply.stdout.trim(), reply.stderr.trim());
                    return None;
                }
                last = unattended::fields(&reply.stdout);
                unattended::unready(&reply.stdout).is_none().then_some(())
            },
        )
        .map_err(|error| error.map(|why| format!("{why}; last trail state: {last}")))
}

/// The unattended take's `trail status` reply as its window `edge` (opened, ended), refused where
/// the request failed; its fields are [`unattended::decide`]'s to judge.
fn unattended_trail(ctl: &Ctl, edge: &str) -> Result<String, CouldNotRun> {
    let trail = ctl.call(&["trail", "status"]);
    if trail.ok() {
        Ok(trail.stdout)
    } else {
        could_not_run(format!(
            "the unattended window gave no trail status as its window {edge} ({} {})",
            trail.stdout.trim(),
            trail.stderr.trim()
        ))
    }
}

/// Whether a windowed Rewound take's window reads [`FOCUSED`] as its measured window `edge`
/// (opened or ended): an unfocused window blinks no caret, so its 0 blink fires decide nothing.
fn rewound_focused(ctl: &Ctl, edge: &str) -> Result<(), CouldNotRun> {
    let trail = ctl.call(&["trail", "status"]);
    let (key, want) = FOCUSED;
    let read = field(&trail.stdout, key)
        .filter(|_| trail.ok())
        .unwrap_or("?");
    if read == want {
        return Ok(());
    }
    could_not_run(format!(
        "the rewound window {edge} {key}={read}, not {key}={want}: an unfocused window blinks no \
         caret, so its 0 blink fires decide nothing (trail status: {} {})",
        trail.stdout.trim(),
        trail.stderr.trim()
    ))
}

fn sole_session(list: &str) -> Option<&str> {
    let mut ids = list
        .split_whitespace()
        .filter(|word| aterm_types::tab_id::is_tab_id(word));
    let sid = ids.next()?;
    ids.next().is_none().then_some(sid)
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
/// [`CouldNotRun`] when the row contradicts itself ([`SpinRow::validate`]), first-exec warm-up is
/// unavailable, the instance did not start, its `temporal status` does not read the row's
/// recording ([`require_recording`]), a counter is missing, the
/// claude scene was not the shape it claims (off the alt screen, or its output
/// moved the grid), the window took more than [`MAX_OS_EVENTS`] events other than
/// a redraw (a person's pointer or keys: not idle) or the pet scene's took any
/// ([`os_events_unidle`]), or a pet scene's window was not a focused window's
/// deeply sleeping pet at full motion ([`PET_READY`]), resting as its scene needs
/// ([`rest_unready`]), as it opened or as it ended, or its footprint could not be read, or a
/// windowed Rewound scene's window did not end focused ([`FOCUSED`]) or was not drawn by the GPU
/// renderer, or the unattended scene's window did not come to its state while the instance worked
/// ([`Budget::await_while_working`]) or [`unattended::decide`] refused its take or left it
/// unanswered.
pub fn measure(rig: &Rig, row: &SpinRow) -> Result<SpinReport, CouldNotRun> {
    row.validate()?;
    let _one = super::take(false);
    static WINDOW: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _window = row.scene.windowed().then(|| {
        WINDOW
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    let mut phases = super::instance::Phases::new(format!("row=spin scene={:?}", row.scene));
    super::instance::first_exec(&rig.fake_tui)?;
    let budget = Budget::from_now(if row.scene.pet() {
        PET_BUDGET_SECS
    } else if row.scene == SpinScene::Unattended {
        unattended_budget_secs(row.window)
    } else {
        BUDGET_SECS
    });
    let mut scratch = Scratch::new("atspin")
        .map_err(|e| CouldNotRun::Refused(format!("cannot make a scratch world: {e}")))?;
    if std::env::var_os("ATERM_PAINT_KEEP").is_some() {
        scratch.keep();
    }
    let world = World::prepare(scratch.path(), &head(row))
        .map_err(|e| CouldNotRun::Refused(format!("cannot lay out the scratch world: {e}")))?;
    let inst = if row.scene.windowed() {
        Instance::launch_windowed(&rig.aterm, &world, budget)?
    } else {
        Instance::launch(&rig.aterm, &world, budget)?
    };
    let ctl = &inst.ctl;
    phases.end("launch");
    if row.scene.pet()
        || matches!(
            row.scene,
            SpinScene::Hidden | SpinScene::WindowedRewound | SpinScene::Unattended
        )
    {
        prepare_focus(ctl)?;
    }
    let focused_at = std::time::Instant::now();
    ctl.ok(&["await", "idle", SETTLE_QUIET_MS, "timeout=60000"])?;
    require_recording(ctl, row.recording)?;
    if row.scene == SpinScene::Unattended {
        await_unattended(ctl, inst.pid(), focused_at)?;
        eprintln!(
            "measure spin: {:?} readiness_seconds={:.3}",
            row.scene,
            focused_at.elapsed().as_secs_f64()
        );
        // The readings' change to their background cadence, before the window opens.
        std::thread::sleep(unattended::SETTLE);
    }
    if row.scene.pet() {
        let started = std::time::Instant::now();
        await_pet_ready(ctl.budget(), row.scene, || {
            let reply = ctl.call(&["trail", "status"]);
            if reply.ok() {
                Ok(reply.stdout)
            } else {
                could_not_run(format!(
                    "the pet readiness read failed: {} {}",
                    reply.stdout.trim(),
                    reply.stderr.trim()
                ))
            }
        })?;
        eprintln!(
            "measure spin: {:?} readiness_seconds={:.3}",
            row.scene,
            started.elapsed().as_secs_f64()
        );
    }
    if row.scene == SpinScene::Hidden {
        let trail = ctl.call(&["trail", "status"]);
        if let Some(why) = trail
            .ok()
            .then(|| hidden_unanswered(&trail.stdout))
            .flatten()
        {
            return unanswered(why);
        }
        ctl.ok(&["send", HIDDEN_COUNTER])?;
        ctl.ok(&["key", "enter"])?;
        ctl.ok(&["invoke", "Minimize"])?;
        ctl.await_event("the hidden scene's window to read off every glass", || {
            let reply = ctl.call(&["trail", "status"]);
            (reply.ok() && hidden_unready(&reply.stdout).is_none()).then_some(())
        })?;
        // The minimise's own animation and focus change, before the window's counters open.
        std::thread::sleep(HIDDEN_SETTLE);
    }
    if row.scene.rewound() {
        ctl.ok(&["send", "printf 'rewind spin\\n'"])?;
        ctl.ok(&["key", "enter"])?;
        ctl.ok(&["await", "idle", SETTLE_QUIET_MS, "timeout=60000"])?;
        let open = ctl.ok(&["rewind", "open"])?;
        if field(&open, "view") != Some("open") {
            return could_not_run(format!("the Rewind view did not open ({open})"));
        }
    }
    phases.end("settle");
    ctl.ok(&["metrics", "reset"])?;
    // Window events count from the reset, and the owners' fires from the metrics read after the
    // pet: what an event before the reset left the pet doing (its fade back in, its gaze's heat)
    // either reads here as a pet asking for frames or ended before the fires' count opened. The
    // read's own wake lands in the window, which its wake bound has room for.
    let pet_open = if row.scene.pet() {
        Some(pet_take(ctl, row.scene, "opened")?)
    } else {
        None
    };
    // The unattended window's state as its window opens: one that lost it since the wait would
    // only be refused at the end ([`unattended::decide`]), so it is refused here.
    let unattended_open = if row.scene == SpinScene::Unattended {
        let trail = unattended_trail(ctl, "opened")?;
        if let Some(off) = unattended::unready(&trail) {
            return could_not_run(format!(
                "the unattended window read {off} as its window opened ({})",
                unattended::fields(&trail)
            ));
        }
        Some(trail)
    } else {
        None
    };
    let opened_at = std::time::Instant::now();
    let opened = ctl.ok(&["metrics"])?;
    if row.scene == SpinScene::Hidden {
        hidden_take(ctl, "opened")?;
    }
    if row.scene == SpinScene::WindowedRewound {
        rewound_focused(ctl, "opened")?;
    }
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
    phases.end("open");
    let sampled = if row.scene.windowed() {
        sample(inst.pid(), row.window)?
    } else {
        std::thread::sleep(row.window);
        None
    };
    phases.end("samples");
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
    if row.scene.rewound() {
        let status = ctl.ok(&["rewind", "status"])?;
        if field(&status, "view") != Some("open") {
            return could_not_run(format!("the Rewind view closed in the window ({status})"));
        }
    }
    let metrics = ctl.ok(&["metrics"])?;
    let at_most = opened_at.elapsed();
    let unattended = match &unattended_open {
        Some(opened_trail) => {
            let ended_trail = unattended_trail(ctl, "ended")?;
            Some(unattended::decide(
                row,
                &unattended::UnattendedReadings {
                    at_least: row.window,
                    at_most,
                    opened: &opened,
                    ended: &metrics,
                    opened_trail,
                    ended_trail: &ended_trail,
                },
            )?)
        }
        None => None,
    };
    let pet = if row.scene.pet() {
        Some(pet_take(ctl, row.scene, "ended")?)
    } else {
        None
    };
    let mut backend = None;
    if row.scene == SpinScene::WindowedRewound {
        rewound_focused(ctl, "ended")?;
        let drawn = field(&metrics, "backend").ok_or_else(|| {
            CouldNotRun::Unanswered(format!("ctl metrics reported no backend ({metrics})"))
        })?;
        if drawn != "gpu" {
            return could_not_run(format!(
                "the rewound window was drawn by the {drawn} renderer, not the GPU the shipped \
                 artifact renders on"
            ));
        }
        backend = Some(drawn.to_string());
    }
    if row.scene == SpinScene::Hidden {
        hidden_take(ctl, "ended")?;
    }
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
    let frames = uint_field(&metrics, "frames")
        .map(|n| n.saturating_sub(uint_field(&opened, "frames").unwrap_or(0)));
    if row.max_frames.is_some() && frames.is_none() {
        return unanswered("ctl metrics does not report frames");
    }
    let hidden_parked = uint_field(&metrics, "redraw_hidden_parked")
        .zip(uint_field(&opened, "redraw_hidden_parked"))
        .map(|(end, start)| end.saturating_sub(start));
    if row.min_hidden_parked.is_some() && hidden_parked.is_none() {
        return unanswered("ctl metrics does not report redraw_hidden_parked");
    }
    let arms = after.saturating_sub(before);
    let deck_clock = [
        "deck_clock_ticks",
        "deck_clock_frames",
        "deck_clock_link_ticks",
        "deck_clock_link_fallbacks",
        "deck_clock_link_resumes",
    ]
    .into_iter()
    .map(|key| {
        (
            key,
            uint_field(&metrics, key)
                .zip(uint_field(&opened, key))
                .map(|(end, start)| end.saturating_sub(start)),
        )
    })
    .collect();
    let report = SpinReport {
        row: *row,
        sampled,
        past_deadline_arms: arms,
        stale_arm_heals: uint_field(&metrics, "stale_arm_heals"),
        frames,
        timer_wakes,
        wakes,
        wait_cancelled_wakes: uint_field(&metrics, "wait_cancelled_wakes"),
        turns: uint_field(&metrics, "turns"),
        wakes_by_variant: field(&metrics, "wakes_by_variant").map(str::to_string),
        fires_by_owner: field(&metrics, "deadline_fires_by_owner").map(str::to_string),
        pet_open,
        pet,
        backend,
        os_events,
        os_event_kinds,
        reasons,
        verdict: unattended.as_ref().map_or_else(
            || match judge(
                row,
                Counts {
                    arms,
                    timer_wakes,
                    owner_fires: &fired,
                    needle,
                    wakes,
                    frames,
                    hidden_parked,
                },
            ) {
                ShapeVerdict::Pass => Disposition::Pass,
                ShapeVerdict::Fail => Disposition::Fail,
            },
            |decision| decision.verdict,
        ),
        unattended,
        owner_fires: fired,
        needle,
        hidden_parked,
        deck_clock,
        checks: super::instance::replay_checks(ctl),
    };
    phases.end("fences");
    drop(inst);
    phases.end("stop");
    Ok(report)
}

#[cfg(test)]
#[path = "spin_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "spin_focus_precondition_tests.rs"]
mod focus_precondition_tests;
