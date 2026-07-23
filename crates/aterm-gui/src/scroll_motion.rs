// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SCROLL KINEMATICS (M1) — the pure motion core for pixel-true smooth
//! scrolling: position decomposition, the wheel glide, the overscroll spring,
//! and the auto-fading scroll pill.
//!
//! Everything here is PURE (integer/float math over explicit inputs — no
//! `&Terminal`, no rendering, no clock reads except through the thin `Instant`
//! wrappers), so the M1 PROVE bullets are machine-checked over deliberate
//! input lattices under plain `cargo test`, and the glide's wake discipline
//! has an abstract ty twin (`aterm_spec::derive::scroll_glide_model`, checked
//! by the real Trust `ty` in aterm-spec's `derived_ring_ty`).
//!
//! # Invariants (proven)
//!
//! 1. **Decomposition law** ([`decompose`]): for EVERY `scroll_px: i64` and
//!    `cell_h >= 1`, `rows * cell_h + frac == scroll_px` with
//!    `frac ∈ [0, cell_h)` — the exact row/fractional-pixel split the render
//!    path consumes (`scroll_px_decomposition_law` sweeps a signed px ×
//!    odd/even cell-height lattice; division is outside the ty `Expr`
//!    language, so the lattice test is the always-on proof layer — the
//!    documented waiver, mirroring the box-drawing rounding law).
//! 2. **Glide convergence** ([`glide_position`] / [`Glide`]): the ease-out
//!    reaches its target EXACTLY at `elapsed >= dur` (never beyond it), moves
//!    monotonically toward the target, and the wrapper disarms (`sample`
//!    returns `done`, after which the host drops the state → no armed
//!    deadline, no perpetual wake). Tier-0: `scroll_glide_model` (ty proves
//!    bounded wakes + disarm-only-at-target at `Buggy=0`, and REQUIRES a
//!    counterexample from the wake-without-progress mutant at `Buggy=1`);
//!    Tier-1: the lattice tests below drive this shipping code.
//! 3. **Spring overshoot-freedom** ([`spring_displacement`]): the release
//!    curve is the CRITICALLY DAMPED solution `x(t) = x0·(1+ωt)·e^{-ωt}`
//!    (damping ratio ζ = 1 by construction — the `(1+ωt)e^{-ωt}` form IS the
//!    repeated-root solution from rest). It never changes sign (no
//!    overshoot), its magnitude is monotone non-increasing, and it converges
//!    below any ε in bounded time (so the host's deadline self-disarms).
//!    Transcendental — outside both ty and the exhaustive-integer style — so
//!    the proof layer is a dense time-lattice property test (the documented
//!    waiver).
//!
//! The scroll PILL (geometry + fade) carries its own lattice-proven laws:
//! the thumb always lies within the track, its length never shrinks below
//! the floor, and its position is monotone in the scroll offset with exact
//! endpoints (top of history ⇒ track top, live bottom ⇒ track bottom).
//!
//! All consumers gate through W11's [`MotionPolicy`](crate::motion): Reduced
//! ⇒ the glide is never armed (instant snap, `MotionEffect::SmoothScroll`)
//! and the pill shows/hides without a fade ramp (`MotionEffect::ScrollPill`).

use std::time::{Duration, Instant};

/// Wheel-glide duration: ~180 ms ease-out (the M1 brief's cadence).
pub(crate) const GLIDE_MS: u64 = 180;

/// How long the pill stays fully opaque after the last scroll activity.
pub(crate) const PILL_HOLD_MS: u64 = 900;

/// Fade-out ramp length after the hold (skipped under Reduced motion).
pub(crate) const PILL_FADE_MS: u64 = 300;

/// Critically damped spring rate (rad/s): settles to ~1% in ~350 ms.
pub(crate) const SPRING_OMEGA: f64 = 18.0;

/// Displacement below which the spring is DONE (self-disarm threshold, px).
pub(crate) const SPRING_EPS_PX: f64 = 0.5;

/// Elastic-overscroll settle cap (ms): the LAST wake the host arms for a bounce.
/// `spring_displacement` decays below [`SPRING_EPS_PX`] well within this for any
/// sub-cell amplitude (the proven bounded-settle law — `<= 350 ms` at
/// [`SPRING_OMEGA`] for `|x0| <= one cell`), so [`OverscrollSpring::sample`]
/// self-disarms EARLIER; this is only the hard safety bound that keeps the wake
/// count finite (the 0%-idle discipline, mirroring [`Glide::end`]).
pub(crate) const SPRING_SETTLE_MS: u64 = 400;

/// Decompose an absolute scroll position in pixels into whole rows plus a
/// fractional-pixel remainder.
///
/// # Invariant (proven)
///
/// For every `scroll_px: i64` and `cell_h >= 1`:
/// `rows * cell_h + frac == scroll_px` and `0 <= frac < cell_h`.
/// Euclidean division makes this hold for NEGATIVE positions too (the
/// elastic-overscroll domain), where truncating division would hand back a
/// negative `frac`. Proven over a signed px × odd/even cell-height lattice in
/// `scroll_px_decomposition_law` below.
#[must_use]
pub(crate) fn decompose(scroll_px: i64, cell_h: i64) -> (i64, i64) {
    debug_assert!(cell_h >= 1, "cell height must be positive");
    (scroll_px.div_euclid(cell_h), scroll_px.rem_euclid(cell_h))
}

/// The eased glide position at `elapsed_ms` of a `dur_ms` ease-out-cubic from
/// `start_px` to `target_px`.
///
/// # Invariant (proven)
///
/// * `elapsed_ms >= dur_ms` (or `dur_ms == 0`) returns EXACTLY `target_px` —
///   convergence is by construction, not by float luck.
/// * `elapsed_ms == 0` returns exactly `start_px`.
/// * The result is monotone from `start_px` toward `target_px` in
///   `elapsed_ms` and never oversteps the target (ease-out has no overshoot).
#[must_use]
pub(crate) fn glide_position(start_px: i64, target_px: i64, elapsed_ms: u64, dur_ms: u64) -> i64 {
    if elapsed_ms >= dur_ms || dur_ms == 0 {
        return target_px;
    }
    // u ∈ (0, 1); ease-out cubic e = 1 - (1-u)^3 ∈ (0, 1), monotone in u.
    let u = elapsed_ms as f64 / dur_ms as f64;
    let inv = 1.0 - u;
    let e = 1.0 - inv * inv * inv;
    let delta = target_px - start_px;
    // Rounding a monotone sequence stays monotone; |e| < 1 keeps the step
    // strictly inside [start, target], so the exact-target return above is
    // the ONLY producer of the final position.
    start_px + (delta as f64 * e).round() as i64
}

/// A self-disarming wheel glide: an ease-out from `start_px` to `target_px`
/// over [`GLIDE_MS`]. The host samples it on frame-paced wakes and DROPS it
/// the moment `sample` reports done — after which no deadline is armed (the
/// 0%-idle discipline; the abstract twin is `scroll_glide_model`).
#[derive(Debug)]
pub(crate) struct Glide {
    start_px: i64,
    target_px: i64,
    t0: Instant,
    dur: Duration,
}

impl Glide {
    /// A glide from `start_px` to `target_px` starting at `now`.
    #[must_use]
    pub(crate) fn new(start_px: i64, target_px: i64, now: Instant) -> Self {
        Self {
            start_px,
            target_px,
            t0: now,
            dur: Duration::from_millis(GLIDE_MS),
        }
    }

    /// The eased position at `now`, and whether the glide is DONE (position
    /// == target exactly). Once done the host drops the glide, disarming its
    /// wake — `done` is precisely the "no perpetual wake" bound.
    #[must_use]
    pub(crate) fn sample(&self, now: Instant) -> (i64, bool) {
        let elapsed = now.saturating_duration_since(self.t0);
        let done = elapsed >= self.dur;
        let pos = glide_position(
            self.start_px,
            self.target_px,
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            self.dur.as_millis() as u64,
        );
        (if done { self.target_px } else { pos }, done)
    }

    /// Redirect the glide toward `target_px`, restarting the ease from the
    /// CURRENT sampled position (a mid-glide wheel notch chains smoothly).
    pub(crate) fn retarget(&mut self, target_px: i64, now: Instant) {
        let (pos, _) = self.sample(now);
        self.start_px = pos;
        self.target_px = target_px;
        self.t0 = now;
    }

    /// The glide's current target (absolute px), for chained retargeting.
    #[must_use]
    pub(crate) fn target_px(&self) -> i64 {
        self.target_px
    }

    /// When the glide completes — the LAST wake the host needs to arm.
    #[must_use]
    pub(crate) fn end(&self) -> Instant {
        self.t0 + self.dur
    }
}

/// Critically damped spring displacement at `t_s` seconds after releasing
/// from rest at `x0_px`: `x(t) = x0 · (1 + ωt) · e^{-ωt}`.
///
/// # Invariant (proven)
///
/// The `(1+ωt)e^{-ωt}` envelope is the repeated-root (ζ = 1, critically
/// damped) solution with `v0 = 0`, so for all `t >= 0` the factor lies in
/// `(0, 1]`: the displacement keeps the sign of `x0` (NO overshoot), its
/// magnitude is monotone non-increasing, and it falls below any positive ε
/// in bounded time. Proven over an amplitude × dense-time lattice in
/// `spring_never_overshoots_and_decays` below.
///
/// Consumed by the M1b elastic-overscroll bounce ([`OverscrollSpring`]): the
/// bidirectional sub-row translate makes the sub-cell displacement visible, so
/// this critically-damped release from rest renders as a rubber-band at a history
/// end.
#[must_use]
pub(crate) fn spring_displacement(x0_px: f64, omega: f64, t_s: f64) -> f64 {
    debug_assert!(omega > 0.0);
    let wt = omega * t_s.max(0.0);
    x0_px * (1.0 + wt) * (-wt).exp()
}

/// Map raw overscroll pixels past a history end into the RESISTED elastic
/// displacement: 0.3× with the sign preserved (`|out| <= 0.3·|raw|`,
/// monotone, odd). Integer, so the CPU/GPU translation consumes exact px.
#[must_use]
pub(crate) fn overscroll_resist(raw_px: i64) -> i64 {
    raw_px.signum() * (raw_px.abs() * 3) / 10
}

/// A self-disarming ELASTIC OVERSCROLL bounce: released from a signed sub-cell
/// displacement `x0_px` when a wheel notch pushes past a history end (top or the
/// live bottom), decaying to rest through the proven critically-damped
/// [`spring_displacement`]. The host samples it on frame-paced wakes — feeding the
/// SIGNED `scroll_frac_px` the bidirectional grid-band translate presents — and
/// DROPS it once the sample settles below [`SPRING_EPS_PX`] (or past
/// [`SPRING_SETTLE_MS`]), after which no deadline is armed (0% idle) — exactly the
/// [`Glide`] discipline. Reduced motion never arms one (the caller gates on the
/// `SmoothScroll` motion policy, like the glide).
#[derive(Debug)]
pub(crate) struct OverscrollSpring {
    /// The signed release amplitude (device px): positive bounces the band UP,
    /// negative bounces it DOWN. Re-seeded (from the live displacement) by a
    /// chained notch via [`Self::add_impulse`].
    x0_px: f64,
    t0: Instant,
    dur: Duration,
}

impl OverscrollSpring {
    /// A bounce released from `x0_px` at `now`.
    #[must_use]
    pub(crate) fn new(x0_px: f64, now: Instant) -> Self {
        Self {
            x0_px,
            t0: now,
            dur: Duration::from_millis(SPRING_SETTLE_MS),
        }
    }

    /// The signed displacement at `now` and whether the bounce is DONE — settled
    /// below [`SPRING_EPS_PX`], or past the [`SPRING_SETTLE_MS`] cap. Once done the
    /// host drops the spring, disarming its wake (the "no perpetual wake" bound).
    #[must_use]
    pub(crate) fn sample(&self, now: Instant) -> (f64, bool) {
        let elapsed = now.saturating_duration_since(self.t0);
        let disp = spring_displacement(self.x0_px, SPRING_OMEGA, elapsed.as_secs_f64());
        let done = elapsed >= self.dur || disp.abs() < SPRING_EPS_PX;
        (disp, done)
    }

    /// Re-release the spring with an added impulse from its CURRENT displacement (a
    /// chained overscroll notch stacks onto the live bounce), restarting the decay
    /// at `now`. The amplitude is clamped to `±max_px` so a hard flick can never
    /// build a bounce larger than one cell (the sub-row translate's domain).
    pub(crate) fn add_impulse(&mut self, delta_px: f64, max_px: f64, now: Instant) {
        let (cur, _) = self.sample(now);
        self.x0_px = (cur + delta_px).clamp(-max_px, max_px);
        self.t0 = now;
    }

    /// When the bounce is guaranteed settled — the LAST wake the host arms (drop
    /// the spring there; the self-disarm that returns the loop to pure `Wait`).
    #[must_use]
    pub(crate) fn end(&self) -> Instant {
        self.t0 + self.dur
    }
}

/// Scroll-pill thumb geometry: where the thumb sits on a `track_px`-tall
/// track for a viewport of `view_rows` over `history_rows` of scrollback at
/// `display_offset` (0 = live bottom, `history_rows` = top).
///
/// Returns `None` when there is nothing to indicate (no history / degenerate
/// track). Otherwise `(y_px, len_px)` with the PROVEN laws:
///
/// * containment: `y + len <= track_px`, `len >= min(min_len_px, track_px)`;
/// * proportionality: `len` grows with `view_rows` relative to the total;
/// * monotone position: `y` is non-increasing in `display_offset` (scrolling
///   into history moves the thumb up), with EXACT endpoints — offset 0 puts
///   the thumb flush at the bottom, offset `history_rows` flush at the top.
#[must_use]
pub(crate) fn pill_geometry(
    track_px: u32,
    min_len_px: u32,
    view_rows: u32,
    history_rows: u32,
    display_offset: u32,
) -> Option<(u32, u32)> {
    if history_rows == 0 || track_px == 0 || view_rows == 0 {
        return None;
    }
    let total = u64::from(view_rows) + u64::from(history_rows);
    let proportional = (u64::from(track_px) * u64::from(view_rows) / total) as u32;
    let len = proportional.max(min_len_px).min(track_px);
    let travel = track_px - len;
    let offset = display_offset.min(history_rows);
    // offset 0 (live bottom) → y = travel (flush bottom);
    // offset == history_rows (top) → y = 0 (flush top).
    let y = (u64::from(travel) * u64::from(history_rows - offset) / u64::from(history_rows)) as u32;
    Some((y, len))
}

/// The pill's fade alpha `since_touch_ms` after the last scroll activity.
/// `animated` = the W11 `ScrollPill` motion gate: `true` ramps 255→0 over
/// [`PILL_FADE_MS`] after the hold; `false` (Reduced motion) shows/hides
/// BINARY (255 until the hold expires, then 0 — no fade animation).
///
/// # Invariant (proven)
///
/// `alpha(0) == 255`; alpha is monotone non-increasing in time; it reaches 0
/// by `PILL_HOLD_MS + PILL_FADE_MS` in both modes (the self-disarm bound);
/// under `animated == false` the only values are 255 and 0.
#[must_use]
pub(crate) fn pill_alpha(since_touch_ms: u64, animated: bool) -> u8 {
    if since_touch_ms < PILL_HOLD_MS {
        return 255;
    }
    if !animated {
        return 0;
    }
    let into_fade = since_touch_ms - PILL_HOLD_MS;
    if into_fade >= PILL_FADE_MS {
        return 0;
    }
    // Linear ramp 255→0 across the fade window (integer, monotone).
    (255 - (into_fade * 255) / PILL_FADE_MS) as u8
}

/// Auto-fading scroll-pill state: a timestamp of the last scroll activity
/// plus the pure [`pill_alpha`] envelope. Deadlines self-disarm — once the
/// alpha hits 0, [`Self::next_deadline`] is `None` and the loop returns to
/// pure `Wait` (0% idle).
#[derive(Debug, Default)]
pub(crate) struct PillFade {
    last_touch: Option<Instant>,
}

impl PillFade {
    /// Record scroll activity at `now`: the pill (re)shows fully opaque.
    pub(crate) fn touch(&mut self, now: Instant) {
        self.last_touch = Some(now);
    }

    /// Elapsed-based alpha at `now` (0 when never touched).
    #[must_use]
    pub(crate) fn alpha(&self, now: Instant, animated: bool) -> u8 {
        self.last_touch.map_or(0, |t| {
            let ms =
                u64::try_from(now.saturating_duration_since(t).as_millis()).unwrap_or(u64::MAX);
            pill_alpha(ms, animated)
        })
    }

    /// Whether the pill still shows at `now`.
    #[must_use]
    pub(crate) fn is_active(&self, now: Instant, animated: bool) -> bool {
        self.alpha(now, animated) > 0
    }

    /// The next wake this pill needs: during the opaque hold, ONE wake at the
    /// hold boundary (the alpha is constant — no repaints needed before it);
    /// during the fade ramp, the frame cadence `frame_iv` capped at the ramp
    /// end (whose wake paints the ERASE frame). `None` once invisible — the
    /// self-disarm that keeps an idle session at 0%.
    #[must_use]
    pub(crate) fn next_deadline(
        &self,
        now: Instant,
        animated: bool,
        frame_iv: Duration,
    ) -> Option<Instant> {
        let t = self.last_touch?;
        let hold_end = t + Duration::from_millis(PILL_HOLD_MS);
        if now < hold_end {
            return Some(hold_end);
        }
        if !animated {
            // Binary mode: the hold-boundary wake (>= hold_end here) paints the
            // erase frame; nothing further to arm.
            return None;
        }
        let fade_end = hold_end + Duration::from_millis(PILL_FADE_MS);
        if now < fade_end {
            return Some((now + frame_iv).min(fade_end));
        }
        None
    }

    /// Drop the pill immediately (e.g. the window's content was replaced).
    #[allow(
        dead_code,
        reason = "lifecycle hook for pane-layout transitions; kept with the type"
    )]
    pub(crate) fn clear(&mut self) {
        self.last_touch = None;
    }
}

#[cfg(test)]
mod tests {
    //! M1 PROVE bullets, Tier-1 (real code): lattice/exhaustive proofs of the
    //! decomposition law, glide convergence, spring overshoot-freedom, and
    //! the pill laws — over the SHIPPING kinematics above. The glide's wake
    //! discipline also carries a Tier-0 abstract twin
    //! (`aterm_spec::derive::scroll_glide_model`) checked by the real Trust
    //! `ty` (proves at `Buggy=0`, counterexample at `Buggy=1`); these tests
    //! bind the same invariants to the code that ships. Division and
    //! transcendentals are outside the ty `Expr` language, so the lattices
    //! here are the always-on proof layer for (1) and (3) — the documented
    //! waiver, mirroring the box-drawing rounding law's size-lattice proof.

    use super::*;

    /// Signed px positions crossing zero, cell boundaries, and large history;
    /// cell heights on an odd/even lattice (the same discipline as the
    /// procedural seam gate).
    const CELL_HEIGHTS: [i64; 8] = [1, 2, 3, 7, 8, 15, 16, 33];

    /// PROVE (1) — decomposition law: `rows*cell_h + frac == px` with
    /// `frac ∈ [0, cell_h)` for EVERY point of a signed px lattice × the
    /// cell-height lattice. Includes negative px (elastic overscroll) where
    /// truncating division would violate the frac range — the negative
    /// control below shows exactly that.
    #[test]
    fn scroll_px_decomposition_law() {
        for &ch in &CELL_HEIGHTS {
            // Dense band around zero + coarse sweep across ±3 cells and far out.
            for px in (-3 * ch - 2..=3 * ch + 2).chain([-1_000_003, -65_536, 65_537, 1_000_003]) {
                let (rows, frac) = decompose(px, ch);
                assert_eq!(
                    rows * ch + frac,
                    px,
                    "recomposition must be exact (px={px}, cell_h={ch})"
                );
                assert!(
                    (0..ch).contains(&frac),
                    "frac out of [0, cell_h): px={px} cell_h={ch} frac={frac}"
                );
            }
        }
        // NON-VACUITY: both a nonzero row part and a nonzero frac occur.
        assert_eq!(decompose(17, 8), (2, 1));
        assert_eq!(decompose(-1, 8), (-1, 7), "negative px borrows a full row");
    }

    /// NEGATIVE CONTROL for (1): truncating division (the naive `px / ch`,
    /// `px % ch`) violates the frac range on negative positions — the lattice
    /// above genuinely catches that implementation.
    #[test]
    fn truncating_division_is_caught() {
        let trunc = |px: i64, ch: i64| (px / ch, px % ch);
        let (t_rows, t_frac) = trunc(-1, 8);
        assert_eq!(t_rows * 8 + t_frac, -1, "truncation still recomposes...");
        assert!(
            !(0..8).contains(&t_frac),
            "...but its frac ({t_frac}) leaves [0, 8) — the law rejects it"
        );
    }

    /// PROVE (2), position law: the ease reaches the target EXACTLY at
    /// `elapsed >= dur`, starts exactly at `start`, moves monotonically
    /// toward the target, and never oversteps — over a start/target × step
    /// cadence lattice (both directions, zero-length glides included).
    #[test]
    fn glide_converges_monotonically_and_exactly() {
        let cases: [(i64, i64); 6] = [
            (0, 960),      // downward into history
            (960, 0),      // back to live
            (0, 0),        // degenerate (already there)
            (-64, 128),    // crossing zero
            (7, 8),        // sub-cell hop
            (100_000, -3), // large jump, sign change
        ];
        for (start, target) in cases {
            for step_ms in [1u64, 7, 16, 33] {
                let mut prev = glide_position(start, target, 0, GLIDE_MS);
                assert_eq!(prev, start, "t=0 must sit at start");
                let mut t = 0u64;
                while t < GLIDE_MS + 2 * step_ms {
                    t += step_ms;
                    let pos = glide_position(start, target, t, GLIDE_MS);
                    if target >= start {
                        assert!(pos >= prev, "must not retreat ({start}->{target} @ {t}ms)");
                        assert!(
                            pos <= target,
                            "must not overshoot ({start}->{target} @ {t}ms)"
                        );
                    } else {
                        assert!(pos <= prev, "must not retreat ({start}->{target} @ {t}ms)");
                        assert!(
                            pos >= target,
                            "must not overshoot ({start}->{target} @ {t}ms)"
                        );
                    }
                    prev = pos;
                }
                assert_eq!(
                    glide_position(start, target, GLIDE_MS, GLIDE_MS),
                    target,
                    "elapsed == dur must land EXACTLY on target"
                );
            }
        }
    }

    /// PROVE (2), wake bound + disarm: driving the `Glide` wrapper on a fixed
    /// frame cadence reaches `done` within `ceil(dur/step) + 1` wakes, the
    /// done sample IS the target, and `end()` never exceeds `t0 + GLIDE_MS`
    /// (the last deadline the host arms — dropping the glide there is the
    /// self-disarm the ty model `scroll_glide_model` proves abstractly).
    #[test]
    fn glide_disarms_in_bounded_wakes() {
        let t0 = Instant::now();
        for step_ms in [4u64, 8, 16, 33] {
            let g = Glide::new(0, 960, t0);
            assert_eq!(g.end(), t0 + Duration::from_millis(GLIDE_MS));
            let bound = GLIDE_MS.div_ceil(step_ms) + 1;
            let mut wakes = 0u64;
            let mut now = t0;
            loop {
                now += Duration::from_millis(step_ms);
                wakes += 1;
                assert!(
                    wakes <= bound,
                    "glide must disarm within {bound} wakes at {step_ms}ms cadence"
                );
                let (pos, done) = g.sample(now);
                if done {
                    assert_eq!(pos, 960, "the done sample must BE the target");
                    break;
                }
            }
            // Retarget mid-flight: the chain converges from the sampled point.
            let mut g = Glide::new(0, 960, t0);
            let mid = t0 + Duration::from_millis(90);
            let (mid_pos, mid_done) = g.sample(mid);
            assert!(!mid_done, "half-way through the ease is not done");
            g.retarget(-240, mid);
            assert_eq!(g.target_px(), -240);
            let (pos, done) = g.sample(mid + Duration::from_millis(GLIDE_MS));
            assert!(done && pos == -240, "retargeted glide converges exactly");
            assert!(
                mid_pos > 0 && mid_pos < 960,
                "non-vacuity: the mid sample is a genuine intermediate"
            );
        }
    }

    /// PROVE (3) — spring overshoot-freedom + decay: over an amplitude ×
    /// omega lattice and a dense time sweep, the displacement keeps the sign
    /// of `x0`, its magnitude never increases, and it falls below
    /// `SPRING_EPS_PX` within a bounded settle time (the self-disarm bound).
    #[test]
    fn spring_never_overshoots_and_decays() {
        for x0 in [-480.0f64, -32.0, -1.0, 1.0, 8.5, 120.0, 480.0] {
            for omega in [8.0f64, SPRING_OMEGA, 30.0] {
                let mut prev_mag = x0.abs();
                let mut settled_at: Option<f64> = None;
                // 1ms lattice over 2s — far past any settle bound used here.
                for ms in 0..=2000u32 {
                    let t = f64::from(ms) / 1000.0;
                    let x = spring_displacement(x0, omega, t);
                    assert!(
                        x * x0 >= 0.0,
                        "sign flip = overshoot (x0={x0}, ω={omega}, t={t}: x={x})"
                    );
                    assert!(
                        x.abs() <= prev_mag + 1e-9,
                        "magnitude must be monotone non-increasing (x0={x0}, ω={omega}, t={t})"
                    );
                    prev_mag = x.abs();
                    if settled_at.is_none() && x.abs() < SPRING_EPS_PX {
                        settled_at = Some(t);
                    }
                }
                let settle = settled_at.expect("spring must settle below ε within 2s");
                assert!(
                    settle <= 1.5,
                    "bounded settle: x0={x0} ω={omega} took {settle}s"
                );
            }
        }
        // NEGATIVE CONTROL: an UNDERDAMPED response (ζ<1, the sinusoid
        // cos(ωd·t)·e^{-ζωt}) DOES flip sign — the sign-law above has teeth.
        let underdamped =
            |x0: f64, t: f64| x0 * (0.5 * SPRING_OMEGA * t).cos() * (-0.3 * SPRING_OMEGA * t).exp();
        assert!(
            (0..=2000).any(|ms| underdamped(100.0, f64::from(ms) / 1000.0) < 0.0),
            "control: the underdamped curve overshoots (sign flip)"
        );
    }

    /// OverscrollSpring wiring: the bounce keeps the sign of its release, decays
    /// monotonically toward rest, self-disarms within the settle cap in bounded
    /// frame-paced wakes (never beyond `end()`), and a chained impulse re-releases
    /// from the LIVE displacement (clamped to ±max). This binds the elastic
    /// rubber-band to the same 0%-idle discipline the glide carries.
    #[test]
    fn overscroll_spring_bounces_and_self_disarms() {
        let t0 = Instant::now();
        // A DOWNWARD bounce (negative amplitude, e.g. a top-of-history overscroll).
        let sp = OverscrollSpring::new(-12.0, t0);
        assert_eq!(sp.end(), t0 + Duration::from_millis(SPRING_SETTLE_MS));
        let (d0, done0) = sp.sample(t0);
        assert!(
            (d0 - -12.0).abs() < 1e-9 && !done0,
            "t=0 sits at the release amplitude"
        );
        // Frame-paced wakes converge to done within the settle cap.
        let mut prev_mag = d0.abs();
        let mut wakes = 0u64;
        let mut now = t0;
        let disarm = loop {
            now += Duration::from_millis(16);
            wakes += 1;
            assert!(
                wakes <= SPRING_SETTLE_MS / 16 + 2,
                "must settle in bounded wakes"
            );
            let (d, done) = sp.sample(now);
            assert!(
                d <= 0.0,
                "sign preserved (no overshoot past rest) for a down bounce"
            );
            assert!(
                d.abs() <= prev_mag + 1e-9,
                "magnitude monotone non-increasing"
            );
            prev_mag = d.abs();
            if done {
                break now;
            }
        };
        assert!(
            disarm <= sp.end() + Duration::from_millis(16),
            "disarms by the settle cap"
        );

        // Chained impulse: re-release from the live displacement, clamped to ±max.
        let mut sp = OverscrollSpring::new(6.0, t0);
        let mid = t0 + Duration::from_millis(20);
        let (live, _) = sp.sample(mid);
        sp.add_impulse(6.0, 8.0, mid);
        let (after, _) = sp.sample(mid);
        assert!(
            (after - (live + 6.0).min(8.0)).abs() < 1e-9,
            "impulse stacks, clamped to max"
        );
        assert!(after <= 8.0, "amplitude never exceeds the one-cell clamp");
    }

    /// Overscroll resistance: odd, sign-preserving, monotone, and bounded by
    /// 0.3× — over a signed lattice.
    #[test]
    fn overscroll_resistance_laws() {
        let mut prev = overscroll_resist(-1001);
        for raw in -1000..=1000i64 {
            let r = overscroll_resist(raw);
            assert_eq!(r, -overscroll_resist(-raw), "odd function");
            assert!(r.signum() == raw.signum() || r == 0, "sign preserved");
            assert!(r.abs() * 10 <= raw.abs() * 3, "|out| <= 0.3|in|");
            assert!(r >= prev, "monotone");
            prev = r;
        }
        assert_eq!(overscroll_resist(100), 30, "non-vacuity: 0.3x is genuine");
    }

    /// Pill geometry laws over a track × viewport × history × offset lattice:
    /// containment, length floor, monotone position, exact endpoints.
    #[test]
    fn pill_geometry_laws() {
        for track in [10u32, 64, 479, 1080] {
            for view in [1u32, 24, 60] {
                for hist in [1u32, 5, 240, 100_000] {
                    let mut prev_y: Option<u32> = None;
                    // Offset sweep INCLUDING both endpoints exactly.
                    let offsets = [0, 1, hist / 3, hist / 2, hist.saturating_sub(1), hist];
                    let mut uniq: Vec<u32> = offsets.to_vec();
                    uniq.sort_unstable();
                    uniq.dedup();
                    for off in uniq {
                        let (y, len) = pill_geometry(track, 8, view, hist, off)
                            .expect("history > 0 must yield a pill");
                        assert!(
                            y + len <= track,
                            "containment (t={track} v={view} h={hist})"
                        );
                        assert!(len >= 8.min(track), "length floor");
                        if off == 0 {
                            assert_eq!(y + len, track, "offset 0 sits flush at the bottom");
                        }
                        if off == hist {
                            assert_eq!(y, 0, "offset == history sits flush at the top");
                        }
                        if let Some(p) = prev_y {
                            assert!(y <= p, "y monotone non-increasing in offset");
                        }
                        prev_y = Some(y);
                    }
                }
            }
        }
        // No history / degenerate geometry → no pill.
        assert_eq!(pill_geometry(100, 8, 24, 0, 0), None);
        assert_eq!(pill_geometry(0, 8, 24, 50, 0), None);
        assert_eq!(pill_geometry(100, 8, 0, 50, 0), None);
    }

    /// Pill fade envelope: opaque at touch, monotone non-increasing, zero by
    /// `HOLD+FADE` (self-disarm) — and BINARY under Reduced motion (the W11
    /// "pill without fade animation" clause).
    #[test]
    fn pill_alpha_envelope() {
        for animated in [true, false] {
            assert_eq!(pill_alpha(0, animated), 255, "opaque at touch");
            let mut prev = 255u8;
            for ms in 0..=(PILL_HOLD_MS + PILL_FADE_MS + 50) {
                let a = pill_alpha(ms, animated);
                assert!(a <= prev, "monotone non-increasing at {ms}ms");
                if !animated {
                    assert!(a == 0 || a == 255, "Reduced motion is binary, got {a}");
                }
                prev = a;
            }
            assert_eq!(
                pill_alpha(PILL_HOLD_MS + PILL_FADE_MS, animated),
                0,
                "invisible by HOLD+FADE (the disarm bound)"
            );
        }
        // Non-vacuity: the animated ramp has a genuine intermediate value.
        let mid = pill_alpha(PILL_HOLD_MS + PILL_FADE_MS / 2, true);
        assert!(mid > 0 && mid < 255, "fade ramp passes through mid alphas");
    }

    /// PillFade deadlines: hold-boundary wake while opaque, frame-paced
    /// during the ramp, `None` once invisible — the self-disarming deadline
    /// discipline (every deadline leads to the state that stops arming).
    #[test]
    fn pill_fade_deadlines_self_disarm() {
        let iv = Duration::from_millis(16);
        let t0 = Instant::now();
        let mut p = PillFade::default();
        assert_eq!(
            p.next_deadline(t0, true, iv),
            None,
            "untouched pill arms nothing"
        );
        p.touch(t0);
        // During the hold: ONE wake at the hold boundary (no repaint churn).
        assert_eq!(
            p.next_deadline(t0, true, iv),
            Some(t0 + Duration::from_millis(PILL_HOLD_MS))
        );
        // During the fade: frame-paced, capped at the ramp end.
        let in_fade = t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS / 2);
        let d = p
            .next_deadline(in_fade, true, iv)
            .expect("fading pill arms");
        assert!(d <= t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS));
        assert!(d > in_fade);
        // After the ramp: alpha 0, DISARMED in both modes.
        let after = t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS);
        assert_eq!(p.alpha(after, true), 0);
        assert_eq!(p.next_deadline(after, true, iv), None, "self-disarm");
        // Reduced motion: the hold-boundary wake is the ERASE wake; nothing after.
        let hold_end = t0 + Duration::from_millis(PILL_HOLD_MS);
        assert_eq!(p.next_deadline(hold_end, false, iv), None);
        assert!(!p.is_active(hold_end, false), "binary hide at the boundary");
        assert!(
            p.is_active(hold_end, true),
            "animated pill still fading there"
        );
    }
}
