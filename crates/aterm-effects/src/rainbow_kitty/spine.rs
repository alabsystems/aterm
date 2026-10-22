// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spine: rainbow kitty v2's one answer to "how hard are you typing", and
//! the display follower every visual reads (`docs/design/RAINBOW-KITTY-V2.md`
//! §17.1, §19.1; T4: the spine drives heat, never birth position).
//!
//! * One metric: [`TypingMomentum`] verbatim. Only non-delete printable typing
//!   builds it; a jump never raises it. Deletes and kills drain it (erase
//!   weight is not a separate integrator).
//! * One follower: [`Spine::disp`], read by ribbon width, star counts, meteor
//!   width, caret paint and the kitty's bank. It chases
//!   `clamp01(metric · METRIC_GAIN)`: attack 0.05 s, slam 0.04 s on a rise past
//!   [`DISP_SLAM_RISE`], release τ 0.85 s.
//! * One resume memory: `Spine::disp_peak`, a 7 s half-life of the follower's
//!   peak that floors birth pricing only ([`Spine::birth_disp`]).
//! * The mend is a mark, not a level: a Backspace records an [`EraseMark`]; the
//!   next typed key within [`MEND_WINDOW_S`] after at most [`MEND_MAX_DELETES`]
//!   deletes is a typo fix priced at `max(birth_disp, mark.disp)`. The mark
//!   touches no metric and arms no timer.
//! * Flow is a counter: a `u32` run of keys at or above [`FLOW_KEY_DISP`] with
//!   no delete, and its high-water mark. [`Flow`] is a pure function of it;
//!   an idle spine drops the run when it snaps to zero. Flow re-prices births
//!   and draws nothing itself.
//!
//! Every entry point takes an injected `now`, and [`Spine::update`] is the only
//! place the follower advances, so a frame is a pure function of the event
//! history and `now`. The follower is `dt`-parameterised (`1 − e^(−dt/τ)`), so
//! any refresh rate reaches the same value at the same wall time (T7).

use aterm_time::Instant;

use super::timing::smoothstep01;
use crate::typing_momentum::TypingMomentum;

/// The gain the follower puts on the canonical metric:
/// `target = clamp01(metric · METRIC_GAIN)`, applied only in [`Spine::target`];
/// the raw metric (`trail status momentum=`) stays ungained.
///
/// 1.23 = 0.80 / 0.65: the display climbs as if `TYPING_MOMENTUM_RATE` were
/// 0.80 without changing the family metric other consumers are pinned to. A
/// cold 8 cps hand reaches `disp` 0.5 on key 5, 0.8 on key 10 and 0.95 on key
/// 13. The cost: a metric ≥ 0.813 reads as full, which delays the release by
/// `τ_metric · ln 1.23` = 414 ms after a full stop.
pub const METRIC_GAIN: f32 = 1.23;

/// Ribbon-momentum attack ease, seconds to ~63 % of a rise: a key swells the
/// ribbon's width over ~50 ms instead of popping it in one frame (T3 governs
/// marks, which are born at full brightness).
pub const DISP_ATTACK_S: f32 = 0.05;

/// The slam ease: a big rise (a fling, a celebration drive, a warm resume)
/// lands within a couple of frames. Kept faster than the attack so an event
/// never lands slower than a keystroke.
pub const DISP_SLAM_S: f32 = 0.04;

/// How big a rise counts as a slam. Half the range in one step is not typing;
/// it is an event.
pub const DISP_SLAM_RISE: f32 = 0.5;

/// Ribbon-momentum release τ, seconds: after the last key the rainbow exhales
/// over ~2 s (≈ 2.5 τ), far slower than it swelled.
pub const DISP_RELEASE_TAU: f32 = 0.85;

/// Half-life of the resume memory, sized to a thinking pause: ≥ 78 % of the
/// peak survives 2.5 s, under a quarter survives 15 s.
pub const DISP_PEAK_HALF_LIFE_S: f32 = 7.0;

/// Share of the decayed peak that floors birth pricing ([`Spine::birth_disp`]):
/// a reborn mark opens at 80 % of the band it re-joins; the rest re-accrues.
pub const DISP_PEAK_FLOOR_SHARE: f32 = 0.8;

/// Below this the follower and peak memory snap to exactly zero, so an idle
/// engine disarms on a clean zero (T6; `Spine::at_rest` is an exact
/// comparison). The same threshold ends a slam (see [`Spine::update`]).
pub const DISP_SNAP_ZERO: f32 = 0.005;

/// The lay/flow clock's rate, sweeps per second at full spine. The phase
/// integrates at `dt · PHASE_RATE · disp`, so the specular glint sweeps
/// head→tail in proportion to momentum and freezes when cold.
pub const PHASE_RATE: f32 = 0.85;

/// The phase ring: the clock is kept modulo this so a long session cannot
/// bleed `f32` precision; `phase` and `phase + PHASE_RING` are equivalent to
/// every consumer.
pub const PHASE_RING: f32 = 1024.0;

/// The mend window, seconds: a typed key this soon after the last delete of a
/// run is a typo fix, born at the momentum the delete interrupted
/// ([`Spine::mend`]). A slip is noticed and retyped within 100–600 ms; longer
/// is a thought and re-earns its price.
pub const MEND_WINDOW_S: f32 = 1.2;

/// The most deletes a run may hold and still be a typo: one or two. Three
/// deletes is an edit — a word going — and the key after it is priced live.
pub const MEND_MAX_DELETES: u8 = 2;

/// The flow floor: a typed key counts toward the run only when its own birth
/// price ([`Spine::birth_disp`]) is at least this; below it the key breaks the
/// run. A cold 8 cps hand reaches 0.8 at ~key 10, so flow is reached by typing,
/// never by the first key. Read off the birth spine so a key re-joining a
/// paused run resumes it.
pub const FLOW_KEY_DISP: f32 = 0.8;

/// Hot resume after a green command, seconds: how long a successful
/// long-running command leaves the door open. The first typed key inside the
/// window is born at [`VERDICT_BIRTH_DISP`]. One key spends it, so there is
/// nothing to taper.
pub const VERDICT_WINDOW_S: f32 = 5.0;

/// The birth price [`Spine::note_verdict`] floors that one key at: the top of
/// the band. Every consumer of [`Spine::birth_disp`] (ribbon brightness and
/// cell life, the exit swoosh's reach, the meteor's launch `mom`) reads it; no
/// consumer of the honest [`Spine::disp`] does.
pub const VERDICT_BIRTH_DISP: f32 = 1.0;

/// The run length at which [`Flow::heat`] is 1.0 and every consumer is at its
/// flowing price: 24 qualifying keys, ~3.1 s from the first key of a cold
/// 12 cps hand (4.4 s at 8 cps).
pub const FLOW_ENTRY_KEYS: u32 = 24;

/// Keys over which [`Flow::heat`] eases up to 1.0 at [`FLOW_ENTRY_KEYS`], so
/// nothing pops (a third of a second at 12 cps). Below
/// `FLOW_ENTRY_KEYS − FLOW_OPEN_EASE_KEYS` heat is exactly zero and every
/// consumer is byte-identical to no flow.
pub const FLOW_OPEN_EASE_KEYS: u32 = 4;

/// How long one [`Spine::drive`] freezes the combo, seconds. A sing-along drive
/// is maximal momentum by definition, so a key held under a celebration would
/// otherwise climb the ladder at auto-repeat cadence. Drives are re-pinned every
/// frame, so the freeze lifts a quarter second after the last one. Frozen both
/// ways: a key inside the window neither climbs nor breaks the run.
pub const FLOW_FREEZE_S: f32 = 0.25;

/// The surge gate: how far a key's birth price must exceed the previous key's
/// before it is born with a longer crisp edge ([`Spine::surge`]). 0.15 is a
/// change of gear; the steepest step of the 8 cps ramp is 0.13 per key.
pub const SURGE_MIN_RISE: f32 = 0.15;

/// The birth-price rise that buys the whole stretch ([`Spine::surge`] 1.0);
/// `ribbon::edge_cells` turns the 0..1 into cells.
pub const SURGE_FULL_RISE: f32 = 0.25;

/// The flow state: the value on the `trail status` row an agent can hold its
/// turn on, and the number every flowing consumer lerps against.
///
/// A pure read of the [`Spine`]'s run counter and high-water mark. `heat` is 0
/// for the first `FLOW_ENTRY_KEYS − FLOW_OPEN_EASE_KEYS` keys of a run (every
/// consumer is identity), eases over the last [`FLOW_OPEN_EASE_KEYS`], and is
/// exactly 1.0 from the [`FLOW_ENTRY_KEYS`]th key on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flow {
    /// 0..1, the open rather than the climb: exactly 0 below the ease, exactly
    /// 1.0 at and after [`FLOW_ENTRY_KEYS`]. (`combo` is the climb.)
    pub heat: f32,
    /// The run: keys typed at [`FLOW_KEY_DISP`] or above with no delete and
    /// no kill since it began. Zero the instant the run breaks.
    pub combo: u32,
    /// The longest run held since the last [`Spine::reset`]; an exit never
    /// lowers it.
    pub best: u32,
}

impl Flow {
    /// Whether the theme is open (the run reached [`FLOW_ENTRY_KEYS`] and has
    /// not broken): `heat == 1.0`, stated on the counter to avoid comparing a
    /// float.
    #[inline]
    #[must_use]
    pub const fn open(&self) -> bool {
        self.combo >= FLOW_ENTRY_KEYS
    }
}

/// [`Flow::heat`] from a run length: the one site of the ease.
#[inline]
#[must_use]
fn flow_heat(run: u32) -> f32 {
    let lo = FLOW_ENTRY_KEYS.saturating_sub(FLOW_OPEN_EASE_KEYS);
    smoothstep01(run.saturating_sub(lo) as f32 / FLOW_OPEN_EASE_KEYS as f32)
}

/// The erase mark a Backspace leaves for the next typed key: the momentum the
/// delete interrupted and the deletes counted since the last typed key.
///
/// A value, not a timer: the key that reads it finds it live or stale
/// ([`Spine::mend`]), and the next typed key clears it either way. `Copy`, so it
/// rides in the spine's snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EraseMark {
    /// The last delete's edge; the window is measured from here.
    pub at: Instant,
    /// The birth spine ([`Spine::birth_disp`]) at the first delete of the run,
    /// kept across the run (a second delete has already drained the follower a
    /// little, and that dip is what the mend closes).
    pub disp: f32,
    /// Deletes in the run since the last typed key (saturating).
    pub count: u8,
}

/// The one momentum integrator and its display follower (see the module doc).
/// `Copy`, like [`TypingMomentum`]: the style-crossfade ghost renders under a
/// frozen snapshot of the thermal state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spine {
    /// The canonical metric (`crate::typing_momentum`), the raw 0..1 leaky
    /// integrator, gained only in [`Spine::target`]; consumers want
    /// [`Spine::disp`].
    momentum: TypingMomentum,
    /// The eased display value the art reads. Advanced only in
    /// [`Spine::update`].
    disp: f32,
    /// The resume memory: the follower's recent peak, forgetting on
    /// [`DISP_PEAK_HALF_LIFE_S`]. Floors birth pricing only.
    disp_peak: f32,
    /// The lay/flow clock in dimensionless sweeps, modulo [`PHASE_RING`].
    phase: f32,
    /// The last [`Spine::update`] stamp; `None` means never ticked, so the first
    /// update only latches the clock. Keeping it here rather than taking `dt`
    /// from callers makes the follower frame-rate independent.
    at: Option<Instant>,
    /// Latched on the tick a rise past [`DISP_SLAM_RISE`] is first seen and held
    /// until the follower is within [`DISP_SNAP_ZERO`] of its target (or the
    /// target falls below it). A property of the edge, not the tick cadence:
    /// choosing the ease afresh each tick would switch at different frames on
    /// 60 Hz and 120 Hz and break T7.
    slamming: bool,
    /// The pending [`EraseMark`], if the last edge was a Backspace. Cleared by
    /// the next typed advance, a kill, and [`Spine::reset`].
    mark: Option<EraseMark>,
    /// The flow run: keys typed at [`FLOW_KEY_DISP`] or above with no delete
    /// since it began. [`Flow`] is a pure function of it.
    run: u32,
    /// The run's high-water mark, kept across exits.
    best: u32,
    /// The birth price the previous typed key was born at, the other half of
    /// the surge's `Δ` ([`Spine::surge`]). Zero before the first key.
    prev_birth: f32,
    /// The last key's own rise over `prev_birth`, latched at its edge so the
    /// tick that births the key reads that key's number.
    key_rise: f32,
    /// The last [`Spine::drive`] with a live floor; the combo is frozen for
    /// [`FLOW_FREEZE_S`] after it.
    driven_at: Option<Instant>,
    /// The hot-resume door: a green long command's stamp, living
    /// [`VERDICT_WINDOW_S`], which floors one birth at [`VERDICT_BIRTH_DISP`]
    /// and is spent by it ([`Spine::note_keys`]). `None` otherwise.
    verdict: Option<Instant>,
}

impl Spine {
    /// A fresh spine at rest.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -- the metric's edges (no follower motion happens here) ---------------

    /// One non-delete printable typing advance (a forward glyph echo, a wrap, a
    /// coalesced echo): the only thing that builds momentum (T1).
    ///
    /// It also closes the erase run, so read [`Spine::mend`] first on a key that
    /// may be a fix. The flow run climbs in [`Spine::note_keys`], not here.
    #[inline]
    pub fn advance(&mut self, now: Instant) {
        self.momentum.advance(now);
        self.mark = None;
    }

    /// The flow run's one edge: `keys` typed advances landed on this frame,
    /// priced at `price` (`Ctx::birth_disp`).
    ///
    /// Called from [`super::Engine::tick`] after [`Spine::update`], not from
    /// [`Spine::advance`]: at the key's edge `disp` is still the last frame's.
    /// Keys coalesced into one frame share its price. A key at
    /// [`FLOW_KEY_DISP`] or above extends the run, one under it breaks it; a
    /// frame within [`FLOW_FREEZE_S`] of a [`Spine::drive`] does neither.
    pub fn note_keys(&mut self, now: Instant, keys: u32, price: f32) {
        if keys == 0 {
            return;
        }
        // The hot-resume door is spent by the key that used it, here where the
        // birth was priced. Spent before the freeze check: a key inside a
        // celebration's freeze still consumes it.
        self.verdict = None;
        if self
            .driven_at
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() < FLOW_FREEZE_S)
        {
            return;
        }
        // `price >= floor`, not `!(price < floor)`: a non-finite spine must
        // break the run.
        if price >= FLOW_KEY_DISP {
            self.run = self.run.saturating_add(keys);
            self.best = self.best.max(self.run);
        } else {
            self.run = 0;
        }
        self.key_rise = price - self.prev_birth;
        self.prev_birth = price;
    }

    /// One Backspace. Deletes never build: this drains
    /// `TYPING_MOMENTUM_DELETE_DRAIN` and spends the pending gap credit, so a
    /// correction dents an earned run and a held delete walks it to zero.
    ///
    /// It leaves the [`EraseMark`]: the birth spine at this edge (before the
    /// follower sees the drain), the run's count one higher, the window
    /// restarted at `now`. A delete more than [`MEND_WINDOW_S`] after the
    /// previous one starts a fresh run. It also breaks the flow run (the
    /// high-water mark is kept).
    pub fn drain_delete(&mut self, now: Instant) {
        self.momentum.delete(now);
        self.run = 0;
        let prior = self
            .mark
            .filter(|m| now.saturating_duration_since(m.at).as_secs_f32() <= MEND_WINDOW_S);
        let here = self.birth_disp();
        self.mark = Some(EraseMark {
            at: now,
            disp: prior.map_or(here, |m| m.disp.max(here)),
            count: prior.map_or(1, |m| m.count.saturating_add(1)),
        });
    }

    /// One kill chord (`^W` / `^U` / `^K`, Alt-D, word backspace): un-earns like
    /// about two deletes. It is an edit, not a typo, so it drops any
    /// [`EraseMark`], and it breaks the flow run.
    pub fn drain_kill(&mut self, now: Instant) {
        self.momentum.kill(now);
        self.mark = None;
        self.run = 0;
    }

    /// The celebration bypass: pin the raw metric to at least `floor` at `now`
    /// (clamped to 0..1; non-finite reads as 0). A held-key sing-along is maximal
    /// flow by definition, so it drives the metric through this one seam and
    /// every legibility cap holds as at earned full momentum.
    pub fn drive(&mut self, now: Instant, floor: f32) {
        let floor = if floor.is_finite() {
            floor.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if floor > 0.0 {
            // The combo freeze is stamped on the drive itself, so a drive under
            // an already hotter spine still freezes the ladder.
            self.driven_at = Some(now);
        }
        if floor > self.momentum.value(now) {
            self.momentum.set_value(now, floor);
        }
    }

    /// The grief gate, [`Spine::drive`]'s inverse: zero the raw metric, the
    /// display follower, the resume memory, the flow run and the hot-resume
    /// door at `now`, keeping `phase` and `best` intact so the family's colour
    /// wheel does not jump and the session's high-water mark survives.
    pub fn hush(&mut self, now: Instant) {
        self.momentum.set_value(now, 0.0);
        self.disp = 0.0;
        self.disp_peak = 0.0;
        self.slamming = false;
        self.mark = None;
        self.run = 0;
        self.prev_birth = 0.0;
        self.key_rise = 0.0;
        self.driven_at = None;
        self.verdict = None;
    }

    // -- the follower ------------------------------------------------------

    /// The one site of the gain: what the follower chases at `now`, the raw
    /// metric times [`METRIC_GAIN`], clamped to 0..1.
    #[inline]
    fn target(&self, now: Instant) -> f32 {
        (self.momentum.value(now) * METRIC_GAIN).clamp(0.0, 1.0)
    }

    /// Advance the display follower, resume memory and lay/flow clock to `now`.
    /// Called exactly once per `Engine::tick`, before any producer reads
    /// [`Spine::disp`], so every mark born on one frame is priced by one number.
    ///
    /// The follower chases [`Spine::target`] exponentially in real elapsed time:
    /// the slam ease from a rise past [`DISP_SLAM_RISE`] until it is reached
    /// (latched), [`DISP_ATTACK_S`] on other rises, [`DISP_RELEASE_TAU`] on a
    /// fall, snapping to zero under [`DISP_SNAP_ZERO`]. Against a held target
    /// each step is the exact first-order solution, so ticks compose. A
    /// backwards or zero `dt` advances nothing.
    pub fn update(&mut self, now: Instant) {
        // The hot-resume door closes by itself on the follower's tick, so a
        // session that goes dark re-earns its price.
        if self
            .verdict
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() > VERDICT_WINDOW_S)
        {
            self.verdict = None;
        }
        let Some(prev) = self.at.replace(now) else {
            // First tick: latch the clock, integrate nothing. `disp` starts at
            // its target so a warm-started fixture needs no ramp frame.
            self.disp = self.target(now);
            self.disp_peak = self.disp;
            return;
        };
        // `saturating_duration_since` can never yield a negative or a NaN, so
        // the only thing to refuse is a zero.
        let dt = now.saturating_duration_since(prev).as_secs_f32();
        if dt <= 0.0 {
            return;
        }
        let target = self.target(now);
        let rise = target - self.disp;
        if rise > DISP_SLAM_RISE {
            self.slamming = true;
        } else if rise < DISP_SNAP_ZERO {
            self.slamming = false;
        }
        let ease = if rise <= 0.0 {
            DISP_RELEASE_TAU
        } else if self.slamming {
            DISP_SLAM_S
        } else {
            DISP_ATTACK_S
        };
        self.disp += rise * (1.0 - (-dt / ease).exp());
        if self.disp < DISP_SNAP_ZERO {
            self.disp = 0.0;
        }
        self.disp_peak = (self.disp_peak
            * (-dt * std::f32::consts::LN_2 / DISP_PEAK_HALF_LIFE_S).exp())
        .max(self.disp);
        if self.disp_peak < DISP_SNAP_ZERO {
            self.disp_peak = 0.0;
        }
        // Idle → exactly zero for flow too (T6): once follower and resume
        // memory have snapped there is no run to re-join, and a held `combo=`
        // would be state with no keystroke behind it.
        if self.disp == 0.0 && self.disp_peak == 0.0 {
            self.run = 0;
            self.prev_birth = 0.0;
            self.key_rise = 0.0;
        }
        // The lay/flow clock: proportional to the spine, wrapped on the exact
        // ring every consumer shares.
        self.phase = (self.phase + dt * PHASE_RATE * self.disp).rem_euclid(PHASE_RING);
    }

    // -- the reads ---------------------------------------------------------

    /// The number the art reads: the eased display spine, 0..1. Ribbon width and
    /// wave, star counts, the meteor's `w`, caret paint and the kitty's bank
    /// resolve through it.
    #[inline]
    #[must_use]
    pub fn disp(&self) -> f32 {
        self.disp
    }

    /// The resume memory: the follower's recent peak, decaying on
    /// [`DISP_PEAK_HALF_LIFE_S`]. Its job is to floor [`Spine::birth_disp`].
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn disp_peak(&self) -> f32 {
        self.disp_peak
    }

    /// The spine that prices a birth: [`Spine::disp`] floored at
    /// [`DISP_PEAK_FLOOR_SHARE`] of the decaying peak.
    ///
    /// Only the birth rules read it (ribbon birth brightness and cell life, the
    /// exit swoosh's reach, the meteor's launch `mom`). Earned-drama consumers
    /// (star envelope, glint, counts) read the honest [`Spine::disp`]: a resume
    /// buys continuity, not fireworks.
    #[inline]
    #[must_use]
    pub fn birth_disp(&self) -> f32 {
        // Floored again by the hot-resume door, which one key spends.
        let verdict = if self.verdict.is_some() {
            VERDICT_BIRTH_DISP
        } else {
            0.0
        };
        self.disp
            .max(DISP_PEAK_FLOOR_SHARE * self.disp_peak)
            .max(verdict)
    }

    /// A command succeeded after a while: open the door for
    /// [`VERDICT_WINDOW_S`]. Draws nothing; it buys a price paid only by a key
    /// the user types. The caller decides what "green" and "a while" mean.
    #[inline]
    pub fn note_verdict(&mut self, now: Instant) {
        self.verdict = Some(now);
    }

    /// Whether the door is open at `now` (diagnostic).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn verdict_open(&self, now: Instant) -> bool {
        self.verdict
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() <= VERDICT_WINDOW_S)
    }

    /// The pending [`EraseMark`], live or stale (diagnostic; nothing is armed
    /// on it).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn erase_mark(&self) -> Option<EraseMark> {
        self.mark
    }

    /// The mend: the [`EraseMark`] if a typed key at `now` is a typo fix (within
    /// [`MEND_WINDOW_S`] of the last delete, at most [`MEND_MAX_DELETES`] in
    /// the run). `None` for a slow correction, a longer run, a kill, or no
    /// delete, and the key is priced at [`Spine::birth_disp`] alone. A pure
    /// read: the engine prices births at `max(birth_disp, mark.disp)` and
    /// publishes the mark in `Ctx::mend`; metric and follower are untouched.
    #[inline]
    #[must_use]
    pub fn mend(&self, now: Instant) -> Option<EraseMark> {
        self.mark.filter(|m| {
            m.count <= MEND_MAX_DELETES
                && now.saturating_duration_since(m.at).as_secs_f32() <= MEND_WINDOW_S
        })
    }

    /// The flow state: the run, its high-water mark and the open ramp
    /// ([`Flow`]). Pure, no clock, no allocation.
    #[inline]
    #[must_use]
    pub fn flow(&self) -> Flow {
        Flow {
            heat: flow_heat(self.run),
            combo: self.run,
            best: self.best,
        }
    }

    /// The surge: how much of the crisp edge's stretch the last typed key
    /// bought, 0..1 (`ribbon::edge_cells` turns it into cells).
    ///
    /// A key whose birth price exceeded the previous key's by at least
    /// [`SURGE_MIN_RISE`] is a change of speed and gets a longer hot edge, in
    /// full at [`SURGE_FULL_RISE`]; so does a key the follower is slamming for
    /// (latched by the [`Spine::update`] preceding the birth). Priced once, at
    /// the key.
    #[inline]
    #[must_use]
    pub fn surge(&self) -> f32 {
        if self.slamming {
            return 1.0;
        }
        if self.key_rise < SURGE_MIN_RISE {
            return 0.0;
        }
        (self.key_rise / SURGE_FULL_RISE).clamp(0.0, 1.0)
    }

    /// The lay/flow clock in sweeps, modulo [`PHASE_RING`]: the rainbow kitty
    /// family's shared phase (`Engine::phase`). Hosts pass it to
    /// `CursorRainbow::tick_with_family_phase` so the caret, its halo and the
    /// ribbon under that cell resolve one spectrum position.
    #[inline]
    #[must_use]
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// The raw canonical metric at `now`: un-eased and un-gained, the number the
    /// kitty cursor's twin instance reads. It differs from [`Spine::disp`] during
    /// attack and release, and under a held hand (`disp` settles on
    /// `min(1, 1.23 · momentum)`).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    /// Idle is exactly zero (T6): true when follower, resume memory and metric
    /// are all at rest at `now`, an exact comparison that [`DISP_SNAP_ZERO`]
    /// makes reachable.
    ///
    /// `Engine::needs_frame_cadence` does not consult this: the resume memory's
    /// 7 s half-life takes ~53 s to snap after a full peak, long after every mark
    /// it priced is gone.
    #[must_use]
    #[cfg(test)]
    pub fn at_rest(&self, now: Instant) -> bool {
        self.disp == 0.0 && self.disp_peak == 0.0 && self.momentum.value(now) == 0.0
    }

    /// Back to rest (layout changes, style switches, `Engine::reset`). Drops the
    /// clock stamp too, so the next [`Spine::update`] latches rather than
    /// integrating across the gap.
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Hot resume: after a green command the first key inside
    /// [`VERDICT_WINDOW_S`] is born at full momentum, one key spends it, and an
    /// unused door closes by itself.
    #[test]
    fn the_first_key_after_a_long_green_run_is_born_hot() {
        let t = Instant::now();

        // A cold spine prices a cold key.
        let mut s = Spine::new();
        s.update(t);
        assert_eq!(
            s.birth_disp(),
            0.0,
            "the shipped identity: with no verdict, nothing is floored"
        );

        // …the build comes back green, and the very next key is at the top of
        // the band.
        s.note_verdict(t);
        s.update(t + ms(200));
        assert!(s.verdict_open(t + ms(200)), "the door is open");
        assert_eq!(
            s.birth_disp(),
            VERDICT_BIRTH_DISP,
            "the first key after a long green run is born hot"
        );

        // …and exactly one key walks through it.
        let price = s.birth_disp();
        s.note_keys(t + ms(200), 1, price);
        assert_eq!(
            s.birth_disp(),
            0.0,
            "the door is spent by the key that walked through it — the SECOND \
             key prices itself honestly"
        );

        // …and a door nobody comes to closes by itself.
        let mut cold = Spine::new();
        cold.note_verdict(t);
        cold.update(t + Duration::from_secs_f32(VERDICT_WINDOW_S + 0.5));
        assert_eq!(
            cold.birth_disp(),
            0.0,
            "five seconds later the build is not news any more"
        );
    }

    /// Hold the follower's target at `target` across ticks: `drive` re-pins the
    /// raw metric at `target / METRIC_GAIN` each tick, so the analytic
    /// first-order solution is exact.
    fn held(s: &mut Spine, from: Instant, to: Instant, step: Duration, target: f32) {
        let floor = target / METRIC_GAIN;
        let mut t = from;
        while t + step <= to {
            t += step;
            s.drive(t, floor);
            s.update(t);
        }
        if t < to {
            s.drive(to, floor);
            s.update(to);
        }
    }

    /// A rise past [`DISP_SLAM_RISE`] takes the faster ease and keeps it until
    /// the rise is reached, so the hand-over does not depend on which frame saw
    /// the gap drop (T7).
    #[test]
    fn a_big_rise_slams_faster_than_it_attacks_and_the_slam_is_latched() {
        let t0 = Instant::now();
        let step = ms(20);

        let slam = {
            let mut x = Spine::new();
            x.update(t0);
            x.drive(t0, 1.0); // rise 1.0 > DISP_SLAM_RISE
            x.update(t0 + step);
            x.disp()
        };
        // A target rise of exactly 0.4 stays under the slam threshold, so it
        // covers a smaller share of its gap in the same time.
        let attack = {
            let mut x = Spine::new();
            x.update(t0);
            x.drive(t0, 0.4 / METRIC_GAIN);
            x.update(t0 + step);
            x.disp() / 0.4
        };
        assert!(
            slam > attack,
            "slam {slam} should cover more of its gap than attack {attack}"
        );

        // Latched: once the remaining gap is under 0.5, the next step still
        // moves at the slam rate, not the attack rate.
        let mut x = Spine::new();
        x.update(t0);
        held(&mut x, t0, t0 + ms(60), ms(10), 1.0);
        let gap = 1.0 - x.disp();
        assert!(
            gap < DISP_SLAM_RISE,
            "the remaining gap is under the threshold"
        );
        x.drive(t0 + ms(70), 1.0);
        x.update(t0 + ms(70));
        let covered = (1.0 - x.disp()) / gap;
        let slam_rate = (-0.010f32 / DISP_SLAM_S).exp();
        let attack_rate = (-0.010f32 / DISP_ATTACK_S).exp();
        assert!(
            (covered - slam_rate).abs() < 1e-4 && (covered - attack_rate).abs() > 1e-3,
            "the step past the threshold must still be a slam step: {covered}"
        );
    }

    /// T6: after typing stops the follower and resume memory reach exactly zero
    /// and `at_rest` latches.
    #[test]
    fn idle_drains_to_exactly_zero() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        s.update(t0);
        let mut t = t0;
        for _ in 0..20 {
            t += ms(30);
            s.advance(t);
            s.update(t);
        }
        assert!(s.disp() > 0.0, "warm before the pause");
        assert!(!s.at_rest(t));

        // 60 s of silence, ticked at a lazy cadence.
        for _ in 0..60 {
            t += ms(1000);
            s.update(t);
        }
        assert_eq!(s.disp(), 0.0, "the follower must reach exact zero");
        assert_eq!(s.disp_peak(), 0.0, "so must the resume memory");
        assert_eq!(s.birth_disp(), 0.0);
        assert!(s.at_rest(t), "idle → zero (T6)");
        // And it stays there under further ticks — no denormal residue.
        t += ms(16);
        s.update(t);
        assert_eq!(s.disp(), 0.0);
    }

    /// Deletes drain and never build; a kill drains harder.
    #[test]
    fn deletes_drain_and_never_build() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        s.update(t0);
        let mut t = t0;
        for _ in 0..30 {
            t += ms(30);
            s.advance(t);
            s.update(t);
        }
        let warm = s.momentum(t);

        let mut deleted = s;
        t += ms(30);
        deleted.drain_delete(t);
        let after_delete = deleted.momentum(t);

        let mut killed = s;
        killed.drain_kill(t);
        let after_kill = killed.momentum(t);

        assert!(after_delete < warm, "a delete must not build");
        assert!(
            after_kill < after_delete,
            "a kill un-earns harder than a delete: {after_kill} vs {after_delete}"
        );
    }

    /// The resume memory floors birth pricing only: after a pause the birth
    /// spine is above the honest spine, which the floor does not touch.
    #[test]
    fn resume_floors_births_without_touching_the_honest_spine() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        s.update(t0);
        let mut t = t0;
        for _ in 0..40 {
            t += ms(25);
            s.advance(t);
            s.update(t);
        }
        let peak = s.disp();
        // A 2.5 s thinking pause.
        for _ in 0..25 {
            t += ms(100);
            s.update(t);
        }
        assert!(s.disp() < peak, "the honest spine decayed");
        assert!(
            s.birth_disp() > s.disp(),
            "the resume memory must floor the birth price"
        );
        assert!(
            s.birth_disp() >= DISP_PEAK_FLOOR_SHARE * s.disp_peak() - 1e-6,
            "the floor is exactly {DISP_PEAK_FLOOR_SHARE} of the decayed peak"
        );
    }

    /// T7: the same history at the same `now` gives the same number at 60 and
    /// 120 Hz, to float rounding under a held target and well under a percent
    /// under a decaying one.
    #[test]
    fn the_follower_is_frame_rate_independent() {
        let t0 = Instant::now();
        let end = t0 + ms(600);
        let run_held = |hz: u64| {
            let mut s = Spine::new();
            s.update(t0);
            held(&mut s, t0, end, ms(1000 / hz), 1.0);
            s.disp()
        };
        let (a, b) = (run_held(60), run_held(120));
        assert!(
            (a - b).abs() < 1e-4,
            "held target: 60 Hz {a} and 120 Hz {b} must agree at the same wall time"
        );

        let run_decaying = |hz: u64| {
            let step = 1000 / hz;
            let mut s = Spine::new();
            s.update(t0);
            s.drive(t0, 1.0);
            let mut t = t0;
            while t + ms(step) <= end {
                t += ms(step);
                s.update(t);
            }
            s.update(end);
            s.disp()
        };
        let (a, b) = (run_decaying(60), run_decaying(120));
        assert!(
            (a - b).abs() < 5e-3,
            "decaying target: 60 Hz {a} and 120 Hz {b} must agree at the same wall time"
        );
    }

    /// The gain's mechanics: under a metric pinned at 0.5 the follower settles on
    /// `0.5 · METRIC_GAIN`; under 0.9 on the ceiling; [`Spine::momentum`]
    /// reports 0.5 and 0.9 (raw) either way.
    #[test]
    fn the_follower_settles_on_the_gained_metric_and_reports_the_raw_one() {
        let t0 = Instant::now();
        for (metric, want) in [(0.5f32, 0.5 * METRIC_GAIN), (0.9, 1.0)] {
            let mut s = Spine::new();
            s.update(t0);
            let mut t = t0;
            for _ in 0..100 {
                t += ms(10);
                s.drive(t, metric);
                s.update(t);
            }
            assert!(
                (s.disp() - want).abs() < 1e-3,
                "metric {metric} held for a second: disp {}, want {want}",
                s.disp()
            );
            assert!(
                (s.momentum(t) - metric).abs() < 1e-6,
                "the raw metric is reported raw: {} vs {metric}",
                s.momentum(t)
            );
        }
    }

    /// The erase mark's arithmetic: the first delete's birth spine is kept across
    /// the run, the count climbs, a third delete is no longer a mend, a typed key
    /// or a kill clears it, a stale run restarts at one, and the follower reads
    /// the same with and without the mark.
    #[test]
    fn a_backspace_leaves_a_mark_that_counts_the_run_and_a_typed_key_closes_it() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        s.update(t0);
        let mut t = t0;
        for _ in 0..16 {
            t += ms(125);
            s.advance(t);
            s.update(t);
        }
        assert_eq!(s.erase_mark(), None, "typing leaves no mark");
        let interrupted = s.birth_disp();
        let metric_before = s.momentum(t + ms(125));

        let mut twin = s;
        t += ms(125);
        s.drain_delete(t);
        twin.drain_delete(t);
        let mark = s.erase_mark().expect("a delete leaves the mark");
        assert_eq!(mark.at, t);
        assert_eq!(mark.count, 1);
        assert!(
            (mark.disp - interrupted).abs() < 1e-6,
            "the mark carries the birth spine the delete interrupted: {} vs {interrupted}",
            mark.disp
        );
        assert!(
            s.momentum(t) < metric_before,
            "the delete still drains the metric"
        );
        assert_eq!(s.mend(t + ms(125)), Some(mark), "one delete is a mend");
        assert_eq!(
            s.mend(t + Duration::from_secs_f32(MEND_WINDOW_S + 0.01)),
            None,
            "…until the window closes"
        );

        // A second delete: the first price is kept, the count is two.
        s.update(t + ms(125));
        t += ms(125);
        s.drain_delete(t);
        let second = s.erase_mark().expect("still marked");
        assert_eq!((second.count, second.at), (2, t));
        assert!(
            (second.disp - interrupted).abs() < 1e-6,
            "the run keeps the momentum its first delete interrupted"
        );
        assert!(s.mend(t + ms(125)).is_some(), "two deletes are a mend");

        // A third: counted, and no longer a mend.
        t += ms(125);
        s.drain_delete(t);
        assert_eq!(s.erase_mark().map(|m| m.count), Some(3));
        assert_eq!(s.mend(t), None, "three deletes are an edit");

        // A typed key closes the run.
        t += ms(125);
        s.advance(t);
        assert_eq!(s.erase_mark(), None, "a typed key clears the mark");

        // A kill drops a run outright.
        t += ms(125);
        twin.drain_kill(t);
        assert_eq!(twin.erase_mark(), None, "a kill is an edit, not a typo");

        // A delete after a stale run starts over at one.
        let mut stale = Spine::new();
        stale.update(t0);
        stale.drain_delete(t0);
        stale.update(t0 + ms(2000));
        stale.drain_delete(t0 + ms(2000));
        assert_eq!(stale.erase_mark().map(|m| m.count), Some(1));

        // The follower is not touched by the mark: the same edges with the
        // mark read and without it are the same number.
        let mut a = Spine::new();
        let mut b = Spine::new();
        a.update(t0);
        b.update(t0);
        for i in 1..=8u32 {
            let at = t0 + ms(125) * i;
            a.advance(at);
            b.advance(at);
            a.update(at);
            b.update(at);
        }
        let at = t0 + ms(125 * 9);
        a.drain_delete(at);
        b.drain_delete(at);
        let _ = a.mend(at);
        a.update(at + ms(125));
        b.update(at + ms(125));
        assert_eq!(a.disp(), b.disp());
        assert_eq!(a.momentum(at + ms(125)), b.momentum(at + ms(125)));
    }

    /// Type `n` keys at `period` through the spine the way the engine does —
    /// the key's edge, then the frame's tick — on a 120 Hz train, and return
    /// the [`Flow`] after each key.
    fn typing(s: &mut Spine, t0: Instant, n: u32, period: Duration) -> Vec<(Duration, Flow)> {
        let tick = Duration::from_micros(8_333);
        let mut out = Vec::new();
        let mut t = t0;
        s.update(t0);
        for k in 1..=n {
            let key = t0 + period * k;
            while t + tick <= key {
                t += tick;
                s.update(t);
            }
            t = key;
            s.advance(t);
            s.update(t);
            let price = s.birth_disp();
            s.note_keys(t, 1, price);
            out.push((key.saturating_duration_since(t0), s.flow()));
        }
        out
    }

    /// A held key cannot farm the combo ([`FLOW_FREEZE_S`]): an armed sing-along
    /// drives the spine up, and its keys neither climb nor break the run; the
    /// hand's earned run is where it left it when the celebration lifts.
    #[test]
    fn a_held_key_cannot_farm_the_combo() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        // A real hand, at 12 cps, to an open theme.
        let earned = typing(&mut s, t0, 40, ms(83));
        let held = earned.last().expect("keys").1;
        assert!(held.open(), "the fixture must reach flow first: {held:?}");

        // …then the key is held: the host drives the spine every frame (the
        // sing-along arm) and the auto-repeat floods keys at 30 cps.
        let mut t = t0 + ms(83) * 40;
        let mut repeats = 0u32;
        for _ in 0..90 {
            t += ms(33);
            s.drive(t, 1.0);
            s.advance(t);
            s.update(t);
            let price = s.birth_disp();
            s.note_keys(t, 1, price);
            repeats += 1;
        }
        assert_eq!(
            s.flow().combo,
            held.combo,
            "{repeats} auto-repeat keys under a celebration moved the combo from {} to {}",
            held.combo,
            s.flow().combo
        );
        assert_eq!(s.flow().best, held.best, "…and the high-water mark with it");

        // The freeze lifts a quarter second after the last drive: the next
        // real key climbs again, on the run the hand had earned.
        let after = t + Duration::from_secs_f32(FLOW_FREEZE_S) + ms(1);
        s.advance(after);
        s.update(after);
        let price = s.birth_disp();
        s.note_keys(after, 1, price);
        assert_eq!(s.flow().combo, held.combo + 1, "the freeze never lifted");
    }

    /// The lay/flow clock advances only while the spine is lit, and wraps on
    /// the exact shared ring.
    #[test]
    fn the_phase_clock_rides_the_spine_and_stays_on_its_ring() {
        let t0 = Instant::now();
        let mut cold = Spine::new();
        cold.update(t0);
        cold.update(t0 + ms(500));
        assert_eq!(cold.phase(), 0.0, "a cold spine freezes the clock");

        let mut hot = Spine::new();
        hot.update(t0);
        hot.drive(t0, 1.0);
        let mut t = t0;
        for _ in 0..200 {
            t += ms(16);
            hot.update(t);
        }
        assert!(hot.phase() > 0.0, "a lit spine advances the clock");
        assert!(
            (0.0..PHASE_RING).contains(&hot.phase()),
            "phase must stay on the ring"
        );
    }
}
