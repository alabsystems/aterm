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
    scroll_px
        .saturating_add(cell_h / 2)
        .div_euclid(cell_h)
        .saturating_mul(cell_h)
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
    let delta = i128::from(target_px) - i128::from(start_px);
    let step = (delta as f64 * e).round();
    let step_i128 = if step.is_finite() { step as i128 } else { 0 };
    let (lo, hi) = (
        i128::from(start_px.min(target_px)),
        i128::from(start_px.max(target_px)),
    );
    (i128::from(start_px) + step_i128).clamp(lo, hi) as i64
}

/// A self-disarming wheel or tracked glide.
#[derive(Debug)]
pub(crate) struct Glide {
    start_px: i64,
    target_px: i64,
    t0: Instant,
    dur: Duration,
    /// Precise deltas: the sub-pixel remainder of the tracked position. Rounding each delta
    /// would lag the finger (at 0.5 rows / 100 ms a 120 Hz delta is 1.42 px, which rounds to
    /// 1 px: 30% slow).
    carry_px: f64,
}

impl Glide {
    /// A glide from `start_px` to `target_px` starting at `now`.
    #[must_use]
    pub(crate) const fn new(start_px: i64, target_px: i64, now: Instant) -> Self {
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
        want_px.saturating_sub(clamped)
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
    pub(crate) const fn settle_start(&self) -> Instant {
        self.t0
    }

    /// Shift start and target by `delta_px` without touching the clock.
    pub(crate) const fn shift(&mut self, delta_px: i64) {
        self.start_px = self.start_px.saturating_add(delta_px);
        self.target_px = self.target_px.saturating_add(delta_px);
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
    pub(crate) const fn target_px(&self) -> i64 {
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
pub(crate) const fn overscroll_resist(raw_px: i64) -> i64 {
    (((raw_px as i128) * 3) / 10) as i64
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
    pub(crate) const fn new(x0_px: f64, now: Instant) -> Self {
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
    pub(crate) const fn next_tick(&self) -> Instant {
        self.next_tick
    }

    /// Re-anchor the next tick.
    pub(crate) const fn set_next_tick(&mut self, next: Instant) {
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
        let limit = if max_px > 0.0 { max_px } else { 0.0 };
        let next = cur + delta_px;
        self.x0_px = if next.is_nan() {
            if cur.is_nan() {
                0.0
            } else {
                cur.clamp(-limit, limit)
            }
        } else {
            next.clamp(-limit, limit)
        };
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
pub(crate) const fn pill_alpha(since_touch_ms: u64, animated: bool) -> u8 {
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
    pub(crate) const fn touch(&mut self, now: Instant) {
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
#[path = "scroll_motion_tests.rs"]
mod tests;
