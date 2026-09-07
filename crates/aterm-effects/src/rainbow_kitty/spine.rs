// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE SPINE** — Rainbow Kitty v2's *one* answer to "how hard are you
//! typing", and the display follower that every visual reads.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §17.1 (`spine.rs`: "ONE momentum
//! integrator … + the display follower … read by ribbon width, star counts,
//! meteor width, caret paint, kitty bank"), §2.1 T4 (velocity is inherited,
//! never smoothed — the spine drives HEAT, never birth position), §19.1 (the
//! five v1 integrators that go).
//!
//! ## The law
//!
//! v1 shipped **five** momentum integrators — the style-shared typing `heat`
//! and its jump `flare` (`cursor_glow.rs:8953`, `:9039`), the eased `disp`
//! spine, the canonical [`TypingMomentum`], a private `erase_mom`, and the
//! `disp_peak` resume memory (`:9990`, `:10180`, `:12543`) — and they
//! disagreed. A jump flare lit the star field with no keystroke behind it; a
//! key-repeat flood maxed `heat` in eight keys; the cat's earn law was a key
//! COUNT. §19.1 deletes all five answers and keeps one:
//!
//! * **ONE metric** — [`TypingMomentum`] verbatim (τ 2.0 s, rate 0.65,
//!   credit cap 0.35 s, delete/kill drains). Builds ONLY from non-delete
//!   printable typing advances; nothing else may raise it, and in particular a
//!   jump never does. That is T1 restated at the metric: no keystroke, no
//!   momentum, therefore no light bought with momentum.
//! * **ONE follower** — the eased display value [`Spine::disp`], which is what
//!   the ribbon width, the star counts, the meteor width (§6.3's `mom`), the
//!   caret paint and the kitty's bank all read. It chases the metric through
//!   ONE gain, `target = clamp01(metric · METRIC_GAIN)` ([`METRIC_GAIN`]
//!   1.23, applied at the one site [`Spine::target`]): v2's display of a
//!   hand is 23 % steeper than the family metric, and the metric itself is
//!   not touched (§23's addendum, 2026-09-06). Attack 0.05 s, slam 0.04 s
//!   on a rise past [`DISP_SLAM_RISE`], release τ 0.85 s: the ribbon
//!   **swells** over ~100 ms and **exhales** over ~2 s, as ONE body.
//! * **ONE resume memory** — [`Spine::disp_peak`], a 7 s half-life of the
//!   follower's own recent peak, which floors BIRTH pricing only
//!   ([`Spine::birth_disp`]). Resume is a resume: a key after a thinking pause
//!   opens at 80 % of the band it re-joins instead of re-earning it from cold
//!   while the owner is already typing at speed.
//! * **ERASE WEIGHT IS A COUNTER, not a sixth integrator** (§19.1). The drains
//!   below move the one metric; nothing accumulates a private erase level.
//!
//! ## Determinism
//!
//! Every entry point takes an injected `now: Instant`; nothing here reads a
//! clock. [`Spine::update`] is the ONLY place the follower advances, so a
//! frame at a given `now` is a pure function of the event history and that
//! `now` (`v2_frames_are_pure_functions_of_now`, §20.1). The follower is
//! `dt`-parameterised (`1 − e^(−dt/τ)`), not per-frame-constant, so a 60 Hz
//! panel and a 120 Hz panel reach the same value at the same wall time — T7.

use aterm_time::Instant;

use crate::typing_momentum::TypingMomentum;

/// The GAIN the follower puts on the canonical metric where the metric forms
/// its target: `target = clamp01(metric · METRIC_GAIN)`, at ONE site
/// ([`Spine::target`]) that the chase, the slam detector, the peak memory
/// and the first-tick latch all read — so every threshold downstream keeps
/// its meaning, and [`Spine::momentum`] (`trail status momentum=`) keeps
/// reporting the RAW metric beside the gained display.
///
/// **1.23 = 0.80 / 0.65 (2026-09-06, §23's addendum "the bigger lever, taken
/// v2-locally").** The momentum audit's one-constant sweep found
/// `TYPING_MOMENTUM_RATE` 0.65 → 0.80 to be the lever 2.5× the size of the
/// attack halving — but that constant is the FAMILY metric: the kitty
/// cursor's earn arc is pinned to it verbatim (§19.1), v1's ribbon reads it,
/// and the casual-burst and program-output laws are stated on it. None of
/// that is v2's to move. The same arc is bought here without moving any of
/// it: what a key EARNS is exactly what it was, and only v2's display of it
/// is steeper by the ratio of the two rates. Measured on the real `Engine`
/// at 120 Hz (the mom-probe, §23), a cold hand at 8 cps reaches `disp` 0.5
/// on key 5 (500 ms after key 1; was key 6, 692 ms), 0.8 on key 10 (1.13 s;
/// was 14, 1.63 s) and 0.95 on key 13 (1.52 s; was 19, 2.28 s); at 12 cps
/// 0.5 on key 6 (450 ms; was 9, 675 ms), 0.95 on key 19 (was 29). That is
/// the rate change's arc to the tick — the same probe with `RATE` 0.8 and no
/// gain reads 500 / 1133 / 1517 ms too. The second key of a cold hand is
/// born at 0.264 (was 0.215); a resume after a 400 ms breath opens at 0.993
/// (was 0.872), after 1 s at 0.893 (was 0.746).
///
/// The price is the ceiling clamp — a metric ≥ 0.813 already reads as
/// full — which is the same trade the rate change makes (`RATE·τ` 1.6
/// clamps at 1.0 from 0.81 of its unclamped curve) and which lands in the
/// top band the eye does not separate (body +39 % vs +35 %). The clamp is
/// also the one thing that MOVES the release: [`DISP_RELEASE_TAU`] is
/// unchanged, but after a full stop the gained target holds the ceiling for
/// `τ_metric · ln 1.23` = 414 ms before it starts down, so the display is
/// −20 % at 1.52 s after the last key (was 1.13 s — by then the swoosh has
/// retracted the ribbon anyway; the exhale shows on the caret and the
/// stars) and reaches exact zero at 12.1 s (was 11.7 s); `at_rest` (the peak
/// memory's 7 s half-life) moves from 53.5 s to 54.0 s. Frame-0 is the
/// ribbon's own envelope and does not move (0.373 of settled on a lone cold
/// key, before and after); the 30 Hz residual of the 8 cps arc is 0.030 of
/// `disp` (was 0.025). Zero allocation — one multiply on the frame path.
pub const METRIC_GAIN: f32 = 1.23;

/// Ribbon-momentum ATTACK ease, seconds to ~63 % of a rise (§17.1: "attack
/// 0.05"). A single key SWELLS the ribbon over ~50 ms instead of popping its
/// width in one frame — the one place in the theme where a *width* eases in,
/// and it is legal because the mark itself is already at full brightness (T3
/// governs marks, not the body they live in).
///
/// **0.05, from 0.10 (2026-09-05, the momentum audit in §23).** The
/// follower lagged the canonical metric by 130–175 ms — a whole key at
/// 8 cps — so the ribbon's price for a key landed one key late. Halving the
/// ease puts the caret's paint and the next key's birth within ~50 ms of the
/// hand: `disp` 0.5 at key 6 instead of 7 (692 ms, was 758) at 8 cps, key 2
/// born at 0.215 instead of 0.168. Every law is untouched (frame-0 is the
/// ribbon's own envelope; off-glass is the swoosh; idle → zero is the release;
/// legibility caps apply after) and the 30 Hz residual stays under 0.03.
pub const DISP_ATTACK_S: f32 = 0.05;

/// …except a SLAM (§17.1: "slam 0.04"): a big rise — a fling, a celebration
/// drive, a warm resume — gets a faster whoosh so the eruption lands within a
/// couple of frames rather than trailing the gesture that bought it. Halved
/// with the attack (2026-09-05) so the slam stays the FASTER of the two —
/// an attack quicker than the slam would make an event land slower than a
/// keystroke.
pub const DISP_SLAM_S: f32 = 0.04;

/// How big a rise counts as a slam. Half the range in one step is not typing;
/// it is an event.
pub const DISP_SLAM_RISE: f32 = 0.5;

/// Ribbon-momentum RELEASE τ, seconds (§17.1: "release 0.85"). The rainbow
/// EXHALES after the last key, dying down far slower than it swelled: ~2 s of
/// visible dimming (≈ 2.5 τ), composing with the ribbon's own profile tail
/// into one long breath. Asymmetric on purpose — attack is response, release
/// is the art (T3).
pub const DISP_RELEASE_TAU: f32 = 0.85;

/// Half-life of the RESUME MEMORY, seconds. Sized to a human thinking pause:
/// ≥ 78 % of the peak survives a 2.5 s pause, under a quarter survives 15 s,
/// so a genuinely cold return re-earns its brightness on the honest metric.
pub const DISP_PEAK_HALF_LIFE_S: f32 = 7.0;

/// The share of the decayed peak that FLOORS birth pricing
/// ([`Spine::birth_disp`]): a reborn mark opens at 80 % of the band it
/// re-joins; the remaining 20 % re-accrues honestly. Up instantly, earned
/// fully.
pub const DISP_PEAK_FLOOR_SHARE: f32 = 0.8;

/// Below this the follower and the peak memory snap to EXACTLY zero, so an
/// idle engine disarms on a clean zero instead of chasing denormal residue —
/// T6, and the reason [`Spine::at_rest`] is an exact comparison and not a
/// tolerance. The same threshold ends a slam: a rise within it of its target
/// is "reached" (see [`Spine::update`]).
pub const DISP_SNAP_ZERO: f32 = 0.005;

/// The lay/flow clock's rate, dimensionless sweeps per second at full spine
/// (v1's `RAINBOW_PHASE_RATE`, unchanged). The phase integrates at
/// `dt · PHASE_RATE · disp`, so the specular glint sweeps head→tail at a rate
/// PROPORTIONAL to momentum and is frozen when cold — where its amplitude is
/// zero anyway.
pub const PHASE_RATE: f32 = 0.85;

/// The phase RING (v1's `RAINBOW_PHASE_RING`, unchanged): the clock is kept
/// modulo this so a multi-hour session cannot bleed `f32` precision, and
/// `phase` and `phase + PHASE_RING` are exactly equivalent to every consumer.
pub const PHASE_RING: f32 = 1024.0;

/// The ONE momentum integrator and its display follower (see the module doc).
///
/// `Copy` for the same reason [`TypingMomentum`] is: the style-crossfade ghost
/// carries a frozen snapshot of the whole thermal state and must keep
/// rendering under it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spine {
    /// THE canonical metric (`crate::typing_momentum`) — the raw 0..1 leaky
    /// integrator. Read through [`Spine::momentum`]; consumers want
    /// [`Spine::disp`] almost always.
    momentum: TypingMomentum,
    /// The eased DISPLAY value the art reads. Advanced only in
    /// [`Spine::update`].
    disp: f32,
    /// The resume memory: the follower's recent peak, forgetting on
    /// [`DISP_PEAK_HALF_LIFE_S`]. Floors BIRTH pricing only.
    disp_peak: f32,
    /// The lay/flow clock in dimensionless sweeps, modulo [`PHASE_RING`].
    phase: f32,
    /// The last [`Spine::update`] stamp; `None` ⇒ never ticked, so the first
    /// update integrates nothing and only latches the clock. Keeping the
    /// stamp here (rather than asking callers for `dt`) is what makes the
    /// follower frame-rate independent without trusting the caller's `dt`.
    at: Option<Instant>,
    /// LATCHED on the tick a rise past [`DISP_SLAM_RISE`] was first seen and
    /// held until the follower is within [`DISP_SNAP_ZERO`] of its target
    /// (or the target falls below it). The slam is a property of the EDGE
    /// that bought it, not of the tick cadence: an ease chosen afresh every
    /// tick from the remaining gap would hand a 60 Hz panel and a 120 Hz
    /// panel the attack constant on different frames, and two different
    /// `disp` values at one wall time — T7 broken by the follower itself.
    slamming: bool,
}

impl Spine {
    /// A fresh spine at rest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -- the metric's edges (no follower motion happens here) ---------------

    /// One NON-DELETE printable typing advance — a forward glyph echo, a wrap,
    /// a coalesced multi-glyph echo. The ONLY thing that builds momentum
    /// (§2.1 T1: a jump, a scroll, program output and a nav hop all build
    /// nothing).
    pub fn advance(&mut self, now: Instant) {
        self.momentum.advance(now);
    }

    /// One Backspace. Deletes never build; this drains
    /// `TYPING_MOMENTUM_DELETE_DRAIN` and spends the pending gap credit, so a
    /// correction dents an earned run without erasing it and a held delete
    /// walks it to zero.
    pub fn drain_delete(&mut self, now: Instant) {
        self.momentum.delete(now);
    }

    /// One kill chord (`^W` / `^U` / `^K`, Alt-D, word-backspace): a span
    /// erased un-earns like ~two deletes.
    pub fn drain_kill(&mut self, now: Instant) {
        self.momentum.kill(now);
    }

    /// THE CELEBRATION BYPASS, kept from v1 and deliberately narrow: pin the
    /// metric to at least `floor` at `now`. An ARMED held-key sing-along IS
    /// maximal flow by definition, so the metric that was rate-normalized to
    /// stop key-repeat floods from out-earning typing is driven straight up
    /// through this ONE seam — no parallel celebration path in the emitters,
    /// so every legibility cap holds at full drive exactly as at earned full
    /// momentum. `floor` is clamped to 0..1; non-finite reads as 0. It pins
    /// the RAW metric, so the follower's target under a drive is the gained
    /// floor, `clamp01(floor · METRIC_GAIN)` — a drive of 1.0 is full either
    /// way.
    pub fn drive(&mut self, now: Instant, floor: f32) {
        let floor = if floor.is_finite() {
            floor.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if floor > self.momentum.value(now) {
            self.momentum.set_value(now, floor);
        }
    }

    // -- the follower ------------------------------------------------------

    /// **THE ONE SITE OF THE GAIN** — what the follower chases at `now`:
    /// the raw metric times [`METRIC_GAIN`], clamped to 0..1. The chase, the
    /// slam detector (a rise of the gained target), the peak memory (of the
    /// follower that chased it) and the first-tick latch all read this and
    /// nothing else reads the multiplied value, so the raw metric is
    /// reported raw everywhere ([`Spine::momentum`], [`Spine::at_rest`]).
    #[inline]
    fn target(&self, now: Instant) -> f32 {
        (self.momentum.value(now) * METRIC_GAIN).clamp(0.0, 1.0)
    }

    /// Advance the display follower, the resume memory and the lay/flow clock
    /// to `now`. Called EXACTLY ONCE per `Engine::tick`, before any producer
    /// reads [`Spine::disp`], so every mark born on one frame is priced by one
    /// number.
    ///
    /// The follower chases the GAINED metric ([`Spine::target`],
    /// [`METRIC_GAIN`]) with an exponential of the real elapsed time — the
    /// SLAM ease from a rise past [`DISP_SLAM_RISE`]
    /// until that rise is reached (latched, see [`Spine`]'s `slamming`), the
    /// [`DISP_ATTACK_S`] ease on any other rise, the [`DISP_RELEASE_TAU`] on
    /// a fall — and snaps to exact zero under [`DISP_SNAP_ZERO`] so idle is
    /// idle. Against a held target every step is the exact solution of the
    /// first-order lag (`gap ← gap·e^(−dt/τ)`), so ticks compose: any cadence
    /// reaches the same value at the same wall time. A backwards or zero `dt`
    /// (a clock the host rewound, two ticks in one instant) advances nothing
    /// rather than integrating garbage.
    pub fn update(&mut self, now: Instant) {
        let Some(prev) = self.at.replace(now) else {
            // First tick: latch the clock, integrate nothing. `disp` starts at
            // its own target so a warm-started fixture does not have to burn
            // a frame ramping.
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
        // The lay/flow clock: proportional to the spine, wrapped on the exact
        // ring every consumer shares.
        self.phase = (self.phase + dt * PHASE_RATE * self.disp).rem_euclid(PHASE_RING);
    }

    // -- the reads ---------------------------------------------------------

    /// **THE NUMBER THE ART READS** — the eased display spine, 0..1. Ribbon
    /// width and wave, star counts, the meteor's `w` (§6.3's `mom` at launch),
    /// caret paint and the kitty's bank all resolve through this one value.
    #[inline]
    #[must_use]
    pub fn disp(&self) -> f32 {
        self.disp
    }

    /// The RESUME MEMORY — the follower's recent peak, decaying on
    /// [`DISP_PEAK_HALF_LIFE_S`]. Diagnostic on its own; its job is to floor
    /// [`Spine::birth_disp`].
    #[inline]
    #[must_use]
    pub fn disp_peak(&self) -> f32 {
        self.disp_peak
    }

    /// **THE SPINE THAT PRICES A BIRTH** — [`Spine::disp`] floored at
    /// [`DISP_PEAK_FLOOR_SHARE`] of the decaying peak.
    ///
    /// ONLY the birth laws read this: the ribbon's birth brightness and cell
    /// life, the exit swoosh's mirrored reach, and the meteor's launch `mom`.
    /// Every "earned drama" consumer — star envelope, glint, counts — keeps
    /// reading the honest [`Spine::disp`], because a resume must not buy
    /// fireworks it did not earn, only continuity.
    #[inline]
    #[must_use]
    pub fn birth_disp(&self) -> f32 {
        self.disp.max(DISP_PEAK_FLOOR_SHARE * self.disp_peak)
    }

    /// The lay/flow clock in dimensionless sweeps, modulo [`PHASE_RING`] — the
    /// rainbow kitty family's shared phase ring (`Engine::phase`, seam point
    /// 7). Hosts hand this exact value to
    /// `CursorRainbow::tick_with_family_phase` so the caret, its halo and the
    /// ribbon under that cell resolve ONE spectrum position.
    #[inline]
    #[must_use]
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// The RAW canonical metric at `now` — the honest, un-eased, UN-GAINED
    /// 0..1, exactly the number the kitty cursor's twin instance reads.
    /// Reported beside [`Spine::disp`] in `trail status` because the two
    /// differ during the attack and the long release — and, since
    /// [`METRIC_GAIN`], at rest under a hand too: `disp` settles on
    /// `min(1, 1.23 · momentum)` — so "why is the ribbon thin" needs to name
    /// which one is low.
    #[inline]
    #[must_use]
    pub fn momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    /// **IDLE IS EXACTLY ZERO** (T6). True when the follower, the resume
    /// memory and the metric are all at rest at `now` — an exact comparison,
    /// never a tolerance, which [`DISP_SNAP_ZERO`] makes reachable.
    ///
    /// This is the spine's OWN rest, and `Engine::needs_frame_cadence`
    /// deliberately does not consult it: the resume memory forgets on a 7 s
    /// half-life (≈ 53 s to the snap after a full peak), long after every
    /// mark it priced is off the glass, and a cadence armed on it would hold
    /// the host awake for a minute of idle. §18's cadence rule is the three
    /// POOLS being non-empty; the spine is a number, not a mark.
    #[must_use]
    pub fn at_rest(&self, now: Instant) -> bool {
        self.disp == 0.0 && self.disp_peak == 0.0 && self.momentum.value(now) == 0.0
    }

    /// Back to rest — layout changes, style switches, `Engine::reset`. Drops
    /// the clock stamp too, so the next [`Spine::update`] latches rather than
    /// integrating across the gap.
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

    /// Hold the follower's TARGET exactly at `target` across ticks: `drive`
    /// re-pins the raw metric at `target / METRIC_GAIN` on every tick, so the
    /// follower chases a frozen target and the analytic first-order-lag
    /// solution is exact — the fixture every ease constant below is pinned
    /// against. Stated on the target, not the metric, so the ease pins do not
    /// move with the gain (the gain has its own pins).
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

    /// Drain the canonical metric to EXACTLY zero at `now` — kills are the
    /// one edge that can lower it, and each takes a fixed slice.
    fn drained(s: &mut Spine, now: Instant) {
        while s.momentum(now) > 0.0 {
            s.drain_kill(now);
        }
    }

    /// §17.1's three follower constants — attack 0.05, slam 0.04, release
    /// 0.85 — pinned ANALYTICALLY against a frozen target, so a transcription
    /// of 0.85 → 0.085 or 0.10 → 1.0 cannot ship green. One time constant of
    /// a first-order lag covers `1 − e⁻¹ = 63.2 %` of its gap, and it is the
    /// constant's own value of elapsed time that must do it.
    #[test]
    fn the_follower_constants_are_the_published_three() {
        let t0 = Instant::now();
        let e1 = 1.0 - (-1.0f32).exp();

        // ATTACK: a 0.4 rise is under the slam threshold. One DISP_ATTACK_S
        // of wall time covers e⁻¹ of the gap.
        let mut a = Spine::new();
        a.update(t0);
        held(&mut a, t0, t0 + ms(50), ms(10), 0.4);
        assert!(
            (a.disp() / 0.4 - e1).abs() < 1e-3,
            "attack τ 0.05: {} of the gap after 50 ms, want {e1}",
            a.disp() / 0.4
        );

        // SLAM: a 1.0 rise. One DISP_SLAM_S covers e⁻¹; after 100 ms it is
        // `1 − e^(−2.5) = 0.918` — the slam is LATCHED for the whole rise,
        // it does not hand over to the attack constant part-way.
        let mut s = Spine::new();
        s.update(t0);
        held(&mut s, t0, t0 + ms(40), ms(10), 1.0);
        assert!(
            (s.disp() - e1).abs() < 1e-3,
            "slam τ 0.04: {} after 40 ms, want {e1}",
            s.disp()
        );
        let mut s = Spine::new();
        s.update(t0);
        held(&mut s, t0, t0 + ms(100), ms(10), 1.0);
        let want = 1.0 - (-2.5f32).exp();
        assert!(
            (s.disp() - want).abs() < 1e-3,
            "slam held to the target: {} after 100 ms, want {want}",
            s.disp()
        );

        // RELEASE: a warm follower over a metric drained to exact zero. One
        // DISP_RELEASE_TAU keeps e⁻¹ of what it had — and 100 ms keeps
        // `e^(−0.1/0.85) = 89 %`, which is the asymmetry stated directly.
        let mut r = Spine::new();
        r.update(t0);
        held(&mut r, t0, t0 + ms(1000), ms(10), 1.0);
        let t = t0 + ms(1000);
        drained(&mut r, t);
        let hot = r.disp();
        assert!(hot > 0.99, "a second of full drive lights the follower");
        let mut one_tau = r;
        one_tau.update(t + ms(850));
        assert!(
            (one_tau.disp() / hot - (-1.0f32).exp()).abs() < 1e-3,
            "release τ 0.85: {} of {hot} after 850 ms",
            one_tau.disp()
        );
        let mut short = r;
        short.update(t + ms(100));
        assert!(
            (short.disp() / hot - (-0.1f32 / DISP_RELEASE_TAU).exp()).abs() < 1e-3,
            "release after 100 ms keeps {} of {hot}",
            short.disp()
        );
    }

    /// The follower's shape under REAL typing, not a held drive: a second of
    /// keys lights it, silence lets it fall, and the fall is slower than the
    /// rise was — the swell-and-exhale of §17.1.
    #[test]
    fn the_follower_attacks_fast_and_releases_slow() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        s.update(t0);
        let mut t = t0;
        for _ in 0..40 {
            t += ms(25);
            s.advance(t);
            s.update(t);
        }
        let hot = s.disp();
        assert!(
            hot > 0.35,
            "a second of typing should light the spine, got {hot}"
        );
        let mut r = s;
        r.update(t + ms(850));
        assert!(r.disp() < hot, "the spine must fall once typing stops");
        assert!(
            r.disp() > 0.30 * hot,
            "release τ 0.85 s must not cliff: {} of {hot}",
            r.disp()
        );
        // The same 100 ms moves a rise much further than it moves a fall.
        let rise_step = {
            let mut x = Spine::new();
            x.update(t0);
            held(&mut x, t0, t0 + ms(100), ms(10), 1.0);
            x.disp()
        };
        let fall_step = {
            let mut x = s;
            x.update(t + ms(100));
            (hot - x.disp()) / hot
        };
        assert!(
            rise_step > fall_step,
            "attack {rise_step} must outrun release {fall_step}"
        );
    }

    /// A rise past [`DISP_SLAM_RISE`] takes the faster ease, and it KEEPS it
    /// until the rise is reached: the ease is latched on the edge, so where
    /// the hand-over happens cannot depend on which frame happened to observe
    /// the gap dropping under 0.5 (T7).
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
        // A rise of exactly 0.4 (the TARGET — the drive pins the raw metric
        // under the gain) stays under the slam threshold, so it must cover
        // a SMALLER share of its own gap in the same wall time.
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

        // Latched: past the point where the REMAINING gap is under 0.5, the
        // next step still moves at the slam rate, not the attack rate.
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

    /// T6, at the spine: after typing stops the follower reaches EXACTLY zero
    /// (not an epsilon), the resume memory follows it, and `at_rest` latches —
    /// which is what lets `needs_frame_cadence()` disarm.
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

    /// Deletes never build and mildly drain; a kill drains harder. The erase
    /// weight is this one metric moving, not a private sixth integrator
    /// (§19.1).
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

    /// The resume memory floors BIRTH pricing only: after a thinking pause the
    /// birth spine is well above the honest spine, and the honest spine is
    /// untouched by the floor.
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

    /// Determinism and frame-rate independence: the same event history
    /// evaluated at the same `now` is the same number however many ticks got
    /// there (T7, and the precondition for `v2_frames_are_pure_functions_of_now`).
    ///
    /// Against a HELD target the steps compose exactly, so 60 Hz and 120 Hz
    /// agree to float rounding; against a DECAYING target (one drive, then
    /// silence) the discretisation of the moving input is the only residual,
    /// and it is bounded well under a percent — the latched slam is what
    /// removed the other cadence dependence, the hand-over frame.
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

    /// **THE GAIN'S LAW** (§23's addendum, 2026-09-06): a cold hand typing
    /// at 8 cps — the real metric under real keys, ticked at 120 Hz exactly
    /// as the engine ticks it — sees the display cross 0.5 on key 5, one key
    /// earlier than the family metric alone put it (key 6, 692 ms after key
    /// 1), while the raw metric at that instant is still under 0.5: the
    /// DISPLAY got there a key early; what the keys earned did not move.
    ///
    /// Fails on the tree before [`METRIC_GAIN`] ("disp 0.5 on key 6, 692 ms
    /// after key 1") and passes after (key 5, 500 ms).
    #[test]
    fn a_cold_hand_reaches_half_momentum_display_a_key_earlier_with_the_gain() {
        let t0 = Instant::now();
        let tick = Duration::from_micros(8_333);
        let mut s = Spine::new();
        s.update(t0); // the host's first idle frame latches the clock
        let mut keys = 0u32;
        let mut first_key = t0;
        let mut crossed = None;
        for i in 1..=240u32 {
            let t = t0 + tick * i;
            if (i - 1) % 15 == 0 {
                s.advance(t);
                keys += 1;
                if keys == 1 {
                    first_key = t;
                }
            }
            s.update(t);
            if s.disp() >= 0.5 {
                let after_key_1 = t.saturating_duration_since(first_key).as_millis();
                crossed = Some((keys, after_key_1, s.momentum(t)));
                break;
            }
        }
        let (keys, at_ms, raw) = crossed.expect("two seconds of 8 cps light the display past 0.5");
        assert!(
            keys <= 5,
            "disp 0.5 on key {keys}, {at_ms} ms after key 1 — the gain puts it on key 5 (was 6)"
        );
        assert!(
            raw < 0.5,
            "the raw metric ({raw}) must not have moved: only the display did"
        );
    }

    /// The gain's mechanics, held still: under a metric pinned at 0.5 the
    /// follower settles on `0.5 · METRIC_GAIN`, not on 0.5; under 0.9 it
    /// settles on the ceiling (the clamp); and [`Spine::momentum`] reports
    /// 0.5 and 0.9 either way — the raw metric, un-gained, which is what
    /// `trail status momentum=` and the kitty cursor's twin instance read.
    /// Fails before the gain (0.5 settles on 0.5).
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
