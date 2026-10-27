// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical typing-momentum leaky integrator in `[0, 1]` shared by
//! [`crate::cursor_glow::CursorGlow`] and [`crate::kitty_cursor::CursorCat`].
//! Accrues `RATE * min(gap, CREDIT_CAP_S)` on non-delete printable advances, drains on
//! deletes/kills, and decays exponentially with time constant [`TYPING_MOMENTUM_TAU`].

use aterm_time::Instant;

/// Exponential decay time constant in seconds.
pub const TYPING_MOMENTUM_TAU: f32 = 2.0;

/// Maximum accrual per wall-clock second of sustained typing.
pub const TYPING_MOMENTUM_RATE: f32 = 0.65;

/// Per-advance credit window in seconds (`RATE * CREDIT_CAP_S` max per key).
pub const TYPING_MOMENTUM_CREDIT_CAP_S: f32 = 0.35;

/// Per-backspace drain.
pub const TYPING_MOMENTUM_DELETE_DRAIN: f32 = 0.12;

/// Kill-chord drain (`2 * TYPING_MOMENTUM_DELETE_DRAIN`).
pub const TYPING_MOMENTUM_KILL_DRAIN: f32 = 2.0 * TYPING_MOMENTUM_DELETE_DRAIN;

/// Floor below which lazily decayed reads snap to `0.0`.
const SNAP_ZERO: f32 = 0.000_5;

/// Leaky typing-momentum integrator in `[0, 1]`.
#[derive(Clone, Copy, Debug)]
pub struct TypingMomentum {
    /// Stored value at `at`.
    value: f32,
    /// Timestamp of the last build or drain (`None` when at rest).
    at: Option<Instant>,
}

impl Default for TypingMomentum {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl TypingMomentum {
    /// Construct a fresh integrator at rest.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: 0.0,
            at: None,
        }
    }

    /// Evaluate at `now`, decaying over [`TYPING_MOMENTUM_TAU`] and snapping below [`SNAP_ZERO`].
    #[inline]
    #[must_use]
    pub fn value(&self, now: Instant) -> f32 {
        if self.value < SNAP_ZERO {
            return 0.0;
        }
        let Some(at) = self.at else {
            return 0.0;
        };
        let dt = now.saturating_duration_since(at).as_secs_f32();
        let v = if dt == 0.0 {
            self.value
        } else {
            self.value * crate::color_math::exp_f32(-dt * (1.0 / TYPING_MOMENTUM_TAU))
        };
        if v < SNAP_ZERO { 0.0 } else { v }
    }

    /// Record one printable advance at `now`, crediting `RATE * min(gap, CREDIT_CAP_S)`.
    #[inline]
    pub fn advance(&mut self, now: Instant) {
        let gap = match self.at {
            Some(at) => now.saturating_duration_since(at).as_secs_f32(),
            None => TYPING_MOMENTUM_CREDIT_CAP_S,
        };
        let credit = TYPING_MOMENTUM_RATE * gap.min(TYPING_MOMENTUM_CREDIT_CAP_S);
        self.value = (self.value(now) + credit).min(1.0);
        self.at = Some(now);
    }

    /// Record one backspace at `now`, draining [`TYPING_MOMENTUM_DELETE_DRAIN`].
    #[inline]
    pub fn delete(&mut self, now: Instant) {
        self.value = (self.value(now) - TYPING_MOMENTUM_DELETE_DRAIN).max(0.0);
        self.at = Some(now);
    }

    /// Record one kill chord at `now`, draining [`TYPING_MOMENTUM_KILL_DRAIN`].
    #[inline]
    pub fn kill(&mut self, now: Instant) {
        self.value = (self.value(now) - TYPING_MOMENTUM_KILL_DRAIN).max(0.0);
        self.at = Some(now);
    }

    /// Pin the metric to `v` (clamped to `[0, 1]`) at `now`.
    #[inline]
    pub const fn set_value(&mut self, now: Instant, v: f32) {
        self.value = v.clamp(0.0, 1.0);
        self.at = Some(now);
    }

    /// Reset the metric to `0.0` with no timestamp.
    #[inline]
    pub const fn reset(&mut self) {
        self.value = 0.0;
        self.at = None;
    }

    /// Analytic instant when the metric crosses `threshold` (`> 0`), or `None` when unstamped.
    #[must_use]
    #[inline]
    pub fn low_crossing(&self, threshold: f32) -> Option<Instant> {
        let at = self.at?;
        if self.value <= threshold {
            return Some(at);
        }
        let elapsed = TYPING_MOMENTUM_TAU * crate::color_math::ln_pos_f32(self.value / threshold);
        Some(at + std::time::Duration::from_secs_f32(elapsed.max(0.0)))
    }

    /// Deadline when [`Self::value`] reads `<= threshold` (`0.0..=1.0`), including [`SNAP_ZERO`].
    #[must_use]
    #[inline]
    pub fn reading_low_crossing(&self, threshold: f32) -> Option<Instant> {
        debug_assert!(threshold.is_finite() && (0.0..=1.0).contains(&threshold));
        let at = self.at?;
        if self.value(at) <= threshold {
            return Some(at);
        }
        let crossing = self.low_crossing(threshold.max(SNAP_ZERO))?;
        Some(crossing + std::time::Duration::from_millis(1))
    }
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn accrual_is_rate_capped_so_key_repeat_cannot_outrun_typing() {
        let t0 = Instant::now();
        let run = |gap_ms: u64| {
            let mut m = TypingMomentum::default();
            m.set_value(t0, 0.0);
            let mut t = t0;
            while t <= t0 + Duration::from_millis(1000) {
                t += Duration::from_millis(gap_ms);
                m.advance(t);
            }
            m.value(t0 + Duration::from_millis(1000 + gap_ms))
        };
        let flood = run(5);
        let sprint = run(30);
        assert!(
            (flood - sprint).abs() < 0.02,
            "flood {flood} vs sprint {sprint}: cadence must not change the build rate"
        );
        let ceiling = TYPING_MOMENTUM_RATE
            * TYPING_MOMENTUM_TAU
            * (1.0 - (-1.05_f32 / TYPING_MOMENTUM_TAU).exp());
        assert!(
            flood <= ceiling + 0.02,
            "flood {flood} exceeds the analytic rate ceiling {ceiling}"
        );
    }

    #[test]
    fn credit_cannot_be_banked_across_a_pause() {
        let t0 = Instant::now();
        let mut m = TypingMomentum::default();
        m.advance(t0);
        let first = m.value(t0);
        m.advance(t0 + Duration::from_secs(10));
        let after_pause = m.value(t0 + Duration::from_secs(10));
        let cap = TYPING_MOMENTUM_RATE * TYPING_MOMENTUM_CREDIT_CAP_S;
        assert!(first <= cap + 1e-4, "a cold first key is one capped credit");
        assert!(
            after_pause <= cap + 0.01,
            "10 s of silence buys no banked credit: {after_pause}"
        );
    }

    #[test]
    fn low_crossing_predicts_the_analytic_decay_instant() {
        let t0 = Instant::now();
        let mut m = TypingMomentum::default();
        m.set_value(t0, 0.9);
        let cross = m.low_crossing(0.14).expect("stamped metric has a crossing");
        assert!(
            (m.value(cross) - 0.14).abs() < 0.005,
            "the metric reads the threshold at its predicted crossing"
        );
        m.set_value(t0, 0.1);
        assert_eq!(m.low_crossing(0.14), Some(t0));
        assert_eq!(TypingMomentum::default().low_crossing(0.14), None);
    }

    #[test]
    fn reading_crossing_reaches_the_actual_floor_including_the_zero_snap() {
        let at = Instant::now();
        for (value, floor) in [
            (0.000_75, 0.0),
            (0.000_75, 0.000_1),
            (0.000_75, 0.000_49),
            (0.000_75, SNAP_ZERO),
            (0.000_75, 0.000_7),
            (SNAP_ZERO, 0.0),
            (0.4, 0.3),
            (1.0, 0.0),
        ] {
            let mut metric = TypingMomentum::default();
            metric.set_value(at, value);
            let crossing = metric.reading_low_crossing(floor).unwrap();
            assert!(metric.value(at) > floor);
            assert!(
                crossing > at,
                "an unmet floor must not re-arm the same instant"
            );
            assert!(metric.value(crossing) <= floor, "{value} -> {floor}");
            let analytic = metric.low_crossing(floor.max(SNAP_ZERO)).unwrap();
            assert_eq!(crossing - analytic, Duration::from_millis(1));
        }

        let mut metric = TypingMomentum::default();
        metric.set_value(at, 0.000_75);
        let retired = metric.low_crossing(0.001).unwrap();
        assert_eq!(retired, at);
        assert!(
            metric.value(retired) > 0.0,
            "the old timer fires before dark"
        );

        metric.set_value(at, 0.0);
        assert_eq!(metric.reading_low_crossing(0.0), Some(at));
        metric.set_value(at, 0.000_4);
        assert_eq!(metric.reading_low_crossing(0.0), Some(at));
        metric.set_value(at, 0.3);
        assert_eq!(metric.reading_low_crossing(0.3), Some(at));
        assert_eq!(TypingMomentum::default().reading_low_crossing(0.0), None);
    }

    #[test]
    fn delete_flood_walks_an_earned_run_to_zero() {
        let t0 = Instant::now();
        let mut m = TypingMomentum::default();
        m.set_value(t0, 1.0);
        let mut t = t0;
        for _ in 0..12 {
            t += Duration::from_millis(60);
            m.delete(t);
        }
        assert_eq!(m.value(t), 0.0, "a held delete run fully un-earns");
        t += Duration::from_millis(60);
        m.advance(t);
        assert!(
            m.value(t) <= TYPING_MOMENTUM_RATE * 0.06 + 1e-4,
            "the delete stamped the clock; no banked credit survives"
        );
    }
}
