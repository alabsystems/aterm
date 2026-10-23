// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scroll kinematics for pixel-true smooth scrolling, overscroll spring, and scroll pill.

use std::time::{Duration, Instant};

/// Wheel-glide ease-out duration (ms).
pub(crate) const GLIDE_MS: u64 = 180;

/// Trackpad sub-row hold duration before rest-settle begins (ms).
pub(crate) const TRACK_REST_MS: u64 = 100;

/// Trackpad rest-settle ease-out duration to the nearest whole row (ms).
pub(crate) const TRACK_SETTLE_MS: u64 = 120;

/// How long the pill stays fully opaque after the last scroll activity.
pub(crate) const PILL_HOLD_MS: u64 = 900;

/// Fade-out ramp length after the hold (skipped under Reduced motion).
pub(crate) const PILL_FADE_MS: u64 = 300;

/// Critically damped spring rate (rad/s).
pub(crate) const SPRING_OMEGA: f64 = 18.0;

/// Displacement below which the spring is done (self-disarm threshold, px).
pub(crate) const SPRING_EPS_PX: f64 = 0.5;

/// Elastic-overscroll settle cap (ms).
pub(crate) const SPRING_SETTLE_MS: u64 = 400;

/// Decompose an absolute scroll position in pixels into whole rows plus fractional remainder.
#[must_use]
pub(crate) fn decompose(scroll_px: i64, cell_h: i64) -> (i64, i64) {
    debug_assert!(cell_h >= 1, "cell height must be positive");
    (scroll_px.div_euclid(cell_h), scroll_px.rem_euclid(cell_h))
}

/// The whole-row position nearest to `scroll_px`.
#[must_use]
pub(crate) fn nearest_row_px(scroll_px: i64, cell_h: i64) -> i64 {
    debug_assert!(cell_h >= 1, "cell height must be positive");
    (scroll_px + cell_h / 2).div_euclid(cell_h) * cell_h
}

/// The eased glide position at `elapsed_ms` of a `dur_ms` ease-out-cubic from `start_px` to `target_px`.
#[must_use]
pub(crate) fn glide_position(start_px: i64, target_px: i64, elapsed_ms: u64, dur_ms: u64) -> i64 {
    if elapsed_ms >= dur_ms || dur_ms == 0 {
        return target_px;
    }
    let u = elapsed_ms as f64 / dur_ms as f64;
    let inv = 1.0 - u;
    let e = 1.0 - inv * inv * inv;
    let delta = target_px - start_px;
    start_px + (delta as f64 * e).round() as i64
}

/// A self-disarming wheel or tracked glide.
#[derive(Debug)]
pub(crate) struct Glide {
    start_px: i64,
    target_px: i64,
    t0: Instant,
    dur: Duration,
    /// Precise deltas: the sub-pixel remainder of the tracked position.
    carry_px: f64,
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
            carry_px: 0.0,
        }
    }

    /// A glide parked at `pos_px` that settles to the nearest whole row after [`TRACK_REST_MS`].
    #[must_use]
    pub(crate) fn tracked(pos_px: i64, cell_h: i64, now: Instant) -> Self {
        let mut g = Self::new(pos_px, pos_px, now);
        g.park(pos_px, cell_h, now);
        g
    }

    /// Park the band at `pos_px` and (re)arm the rest-settle from `now`.
    fn park(&mut self, pos_px: i64, cell_h: i64, now: Instant) {
        self.start_px = pos_px;
        self.target_px = nearest_row_px(pos_px, cell_h);
        self.t0 = now + Duration::from_millis(TRACK_REST_MS);
        self.dur = Duration::from_millis(TRACK_SETTLE_MS);
    }

    /// Move the tracked position by `delta_px`, clamp into `[0, max_px]`, and re-arm rest-settle.
    pub(crate) fn track(&mut self, delta_px: f64, cell_h: i64, max_px: i64, now: Instant) -> i64 {
        let (pos, _) = self.sample(now);
        let want = pos as f64 + self.carry_px + delta_px;
        let whole = want.round();
        let want_px = whole as i64;
        let clamped = want_px.clamp(0, max_px.max(0));
        self.carry_px = if clamped == want_px {
            want - whole
        } else {
            0.0
        };
        self.park(clamped, cell_h, now);
        want_px - clamped
    }

    /// Gesture end: start the rest-settle now.
    pub(crate) fn release(&mut self, now: Instant) {
        if now < self.t0 {
            self.t0 = now;
        }
    }

    /// The next wake for this glide at `now`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn next_wake(&self, now: Instant, frame_iv: Duration) -> Instant {
        if now < self.t0 {
            self.t0
        } else {
            self.end().min(now + frame_iv)
        }
    }

    /// When the ease begins (`t0`).
    #[must_use]
    pub(crate) fn settle_start(&self) -> Instant {
        self.t0
    }

    /// Shift start and target by `delta_px` without touching the clock.
    pub(crate) fn shift(&mut self, delta_px: i64) {
        self.start_px += delta_px;
        self.target_px += delta_px;
    }

    /// The eased position at `now`, and whether the glide is done.
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

    /// Redirect the glide toward `target_px`, restarting the ease from the current sample.
    pub(crate) fn retarget(&mut self, target_px: i64, now: Instant) {
        let (pos, _) = self.sample(now);
        self.start_px = pos;
        self.target_px = target_px;
        self.t0 = now;
        self.dur = Duration::from_millis(GLIDE_MS);
        self.carry_px = 0.0;
    }

    /// The glide's current target (absolute px).
    #[must_use]
    pub(crate) fn target_px(&self) -> i64 {
        self.target_px
    }

    /// When the glide completes.
    #[must_use]
    pub(crate) fn end(&self) -> Instant {
        self.t0 + self.dur
    }
}

/// Critically damped spring displacement `t_s` seconds after release from rest at `x0_px`.
#[must_use]
pub(crate) fn spring_displacement(x0_px: f64, omega: f64, t_s: f64) -> f64 {
    debug_assert!(omega > 0.0);
    let wt = omega * t_s.max(0.0);
    x0_px * (1.0 + wt) * (-wt).exp()
}

/// Map raw overscroll pixels past a history end to the resisted elastic displacement.
#[must_use]
pub(crate) fn overscroll_resist(raw_px: i64) -> i64 {
    raw_px.signum() * (raw_px.abs() * 3) / 10
}

/// A self-disarming elastic-overscroll bounce.
#[derive(Debug)]
pub(crate) struct OverscrollSpring {
    /// The signed release amplitude (device px): positive bounces the band up, negative down.
    x0_px: f64,
    t0: Instant,
    dur: Duration,
    /// The next frame-paced tick: an anchor the host sets one period out in
    /// `release_overscroll` and advances phase-locked in `tick_overscroll`.
    next_tick: Instant,
}

impl OverscrollSpring {
    /// A bounce released from `x0_px` at `now`.
    #[must_use]
    pub(crate) fn new(x0_px: f64, now: Instant) -> Self {
        Self {
            x0_px,
            t0: now,
            dur: Duration::from_millis(SPRING_SETTLE_MS),
            next_tick: now,
        }
    }

    /// The anchored next tick capped at the settle bound.
    #[must_use]
    pub(crate) fn wake_deadline(&self) -> Instant {
        self.end().min(self.next_tick)
    }

    /// The anchored next tick.
    #[must_use]
    pub(crate) fn next_tick(&self) -> Instant {
        self.next_tick
    }

    /// Re-anchor the next tick.
    pub(crate) fn set_next_tick(&mut self, next: Instant) {
        self.next_tick = next;
    }

    /// The signed displacement at `now` and whether the bounce is done.
    #[must_use]
    pub(crate) fn sample(&self, now: Instant) -> (f64, bool) {
        let elapsed = now.saturating_duration_since(self.t0);
        let disp = spring_displacement(self.x0_px, SPRING_OMEGA, elapsed.as_secs_f64());
        let done = elapsed >= self.dur || disp.abs() < SPRING_EPS_PX;
        (disp, done)
    }

    /// Re-release from the current displacement plus `delta_px`, clamped to `±max_px`.
    pub(crate) fn add_impulse(&mut self, delta_px: f64, max_px: f64, now: Instant) {
        let (cur, _) = self.sample(now);
        self.x0_px = (cur + delta_px).clamp(-max_px, max_px);
        self.t0 = now;
    }

    /// When the bounce is guaranteed settled.
    #[must_use]
    pub(crate) fn end(&self) -> Instant {
        self.t0 + self.dur
    }
}

/// Scroll-pill thumb geometry `(y_px, len_px)` on a `track_px`-tall track.
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
    let y = (u64::from(travel) * u64::from(history_rows - offset) / u64::from(history_rows)) as u32;
    Some((y, len))
}

/// The pill's alpha `since_touch_ms` after the last scroll activity.
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
    (255 - (into_fade * 255) / PILL_FADE_MS) as u8
}

/// Auto-fading scroll-pill state.
#[derive(Debug, Default)]
pub(crate) struct PillFade {
    last_touch: Option<Instant>,
}

impl PillFade {
    /// Record scroll activity at `now`.
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

    /// The next wake this pill needs, or `None` once invisible.
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
            // Binary mode: the hold-boundary wake paints the erase frame.
            return None;
        }
        let fade_end = hold_end + Duration::from_millis(PILL_FADE_MS);
        if now < fade_end {
            return Some((now + frame_iv).min(fade_end));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    //! M1 PROVE bullets, Tier-1 (real code): lattice/exhaustive proofs of the
    //! decomposition law, glide convergence, spring overshoot-freedom, and
    //! the pill laws — over the SHIPPING kinematics above.
    use super::*;

    /// Cell heights on an odd/even lattice.
    const CELL_HEIGHTS: [i64; 8] = [1, 2, 3, 7, 8, 15, 16, 33];

    /// PROVE (1) — decomposition law: `rows*cell_h + frac == px` with
    /// `frac ∈ [0, cell_h)` for EVERY point of a signed px lattice × the
    /// cell-height lattice.
    #[test]
    fn scroll_px_decomposition_law() {
        for &ch in &CELL_HEIGHTS {
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
        assert_eq!(decompose(17, 8), (2, 1));
        assert_eq!(decompose(-1, 8), (-1, 7), "negative px borrows a full row");
    }

    /// (2) Position law: exact start and target, monotone, no overstep, over start/target
    /// pairs in both directions × step cadences.
    #[test]
    fn glide_converges_monotonically_and_exactly() {
        let cases: [(i64, i64); 6] = [
            (0, 960),
            (960, 0),
            (0, 0),
            (-64, 128),
            (7, 8),
            (100_000, -3),
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

    /// (2) Wake bound: the `Glide` wrapper on a fixed cadence is done within
    /// `ceil(dur/step) + 1` wakes, its done sample is the target, and `end()` is
    /// `t0 + GLIDE_MS`.
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

    /// A 120 Hz precise stream at constant speed through `Glide::track` moves the band 1:1
    /// (within a pixel, sub-pixel carry included) and evenly (per-frame `max <= 2 * min`, no
    /// zero-motion frame) at 0.5 to 4 rows per 100 ms; after the last delta it holds for
    /// `TRACK_REST_MS` with one wake, then settles to the nearest whole row; `release` starts
    /// the settle at once.
    #[test]
    fn a_precise_stream_tracks_one_to_one() {
        let t0 = Instant::now();
        let cell = 34i64;
        let tick = Duration::from_micros(8_333);
        let max_px = 1_000 * cell;
        for rows_per_100ms in [0.5f64, 1.0, 2.0, 4.0] {
            let px_per_tick = rows_per_100ms * cell as f64 * 8.333 / 100.0;

            let mut g = Glide::tracked(0, cell, t0);
            let mut now = t0;
            let mut finger = 0.0f64;
            let mut prev = 0i64;
            let mut vels: Vec<i64> = Vec::new();
            for i in 1..=120u32 {
                now += tick;
                finger += px_per_tick;
                assert_eq!(
                    g.track(px_per_tick, cell, max_px, now),
                    0,
                    "nothing clamped away far from the edges"
                );
                let (pos, done) = g.sample(now);
                assert!(!done, "a tracked glide is never done while deltas arrive");
                assert!(
                    (pos as f64 - finger).abs() <= 1.0,
                    "1:1 — the band is within a pixel of the finger ({pos} vs {finger:.2} at tick {i}, {rows_per_100ms} rows/100 ms)"
                );
                if i > 12 {
                    vels.push(pos - prev);
                }
                prev = pos;
            }
            let (mx, mn) = (*vels.iter().max().unwrap(), *vels.iter().min().unwrap());
            assert!(
                mn >= 1 && mx <= 2 * mn,
                "even motion at {rows_per_100ms} rows/100 ms: max {mx} px/frame, min {mn}"
            );
            let rest_end = now + Duration::from_millis(TRACK_REST_MS);
            assert_eq!(
                g.next_wake(now, tick),
                rest_end,
                "parked: ONE wake at the rest boundary, not a frame cadence"
            );
            assert_eq!(
                g.sample(rest_end - Duration::from_millis(1)).0,
                prev,
                "the band holds the finger's position through the rest window"
            );
            let end = g.end();
            assert_eq!(end, rest_end + Duration::from_millis(TRACK_SETTLE_MS));
            assert_eq!(
                g.next_wake(rest_end, tick),
                rest_end + tick,
                "settling: frame-paced wakes"
            );
            let (landed, done) = g.sample(end);
            assert!(done, "done at end()");
            assert_eq!(landed % cell, 0, "…on a whole row");
            assert!(
                (landed - prev).abs() <= cell / 2,
                "…the NEAREST one ({landed} from {prev})"
            );
            let mut r = Glide::tracked(7, cell, t0);
            r.release(t0 + Duration::from_millis(3));
            assert_eq!(
                r.end(),
                t0 + Duration::from_millis(3 + TRACK_SETTLE_MS),
                "release moves the settle to now"
            );
            let mut e = Glide::new(0, cell, t0);
            e.release(t0 + Duration::from_millis(3));
            assert_eq!(
                e.end(),
                t0 + Duration::from_millis(GLIDE_MS),
                "a notch glide ignores release"
            );
        }
    }

    /// (3) Spring: over an amplitude × omega lattice and a dense time sweep, the displacement
    /// keeps the sign of `x0`, never grows in magnitude, and falls below `SPRING_EPS_PX` in
    /// bounded time.
    #[test]
    fn spring_never_overshoots_and_decays() {
        for x0 in [-480.0f64, -32.0, -1.0, 1.0, 8.5, 120.0, 480.0] {
            for omega in [8.0f64, SPRING_OMEGA, 30.0] {
                let mut prev_mag = x0.abs();
                let mut settled_at: Option<f64> = None;
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
        let underdamped =
            |x0: f64, t: f64| x0 * (0.5 * SPRING_OMEGA * t).cos() * (-0.3 * SPRING_OMEGA * t).exp();
        assert!(
            (0..=2000).any(|ms| underdamped(100.0, f64::from(ms) / 1000.0) < 0.0),
            "control: the underdamped curve overshoots (sign flip)"
        );
    }

    /// The bounce keeps its sign, decays monotonically, is done within the settle cap in
    /// bounded wakes, and a chained impulse re-releases from the live displacement, clamped
    /// to ±max.
    #[test]
    fn overscroll_spring_bounces_and_self_disarms() {
        let t0 = Instant::now();
        let sp = OverscrollSpring::new(-12.0, t0);
        assert_eq!(sp.end(), t0 + Duration::from_millis(SPRING_SETTLE_MS));
        let (d0, done0) = sp.sample(t0);
        assert!(
            (d0 - -12.0).abs() < 1e-9 && !done0,
            "t=0 sits at the release amplitude"
        );
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
        assert_eq!(pill_geometry(100, 8, 24, 0, 0), None);
        assert_eq!(pill_geometry(0, 8, 24, 50, 0), None);
        assert_eq!(pill_geometry(100, 8, 0, 50, 0), None);
    }

    /// Pill fade: opaque at touch, monotone non-increasing, zero by `HOLD+FADE`, and binary
    /// under Reduced motion.
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
        let mid = pill_alpha(PILL_HOLD_MS + PILL_FADE_MS / 2, true);
        assert!(mid > 0 && mid < 255, "fade ramp passes through mid alphas");
    }

    /// PillFade deadlines: a hold-boundary wake while opaque, frame-paced during the ramp,
    /// `None` once invisible.
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
        assert_eq!(
            p.next_deadline(t0, true, iv),
            Some(t0 + Duration::from_millis(PILL_HOLD_MS))
        );
        let in_fade = t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS / 2);
        let d = p
            .next_deadline(in_fade, true, iv)
            .expect("fading pill arms");
        assert!(d <= t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS));
        assert!(d > in_fade);
        let after = t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS);
        assert_eq!(p.alpha(after, true), 0);
        assert_eq!(p.next_deadline(after, true, iv), None, "self-disarm");
        let hold_end = t0 + Duration::from_millis(PILL_HOLD_MS);
        assert_eq!(p.next_deadline(hold_end, false, iv), None);
        assert!(!p.is_active(hold_end, false), "binary hide at the boundary");
        assert!(
            p.is_active(hold_end, true),
            "animated pill still fading there"
        );
    }
}
