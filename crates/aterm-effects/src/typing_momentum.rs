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

    #[inline]
    const fn value_after(&self, dt: f32) -> f32 {
        if self.value < SNAP_ZERO {
            return 0.0;
        }
        let v = if dt == 0.0 {
            self.value
        } else {
            self.value * crate::color_math::exp_f32(-dt * (1.0 / TYPING_MOMENTUM_TAU))
        };
        if v < SNAP_ZERO { 0.0 } else { v }
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
        self.value_after(now.saturating_duration_since(at).as_secs_f32())
    }

    /// Record one printable advance at `now`, returning `(decayed_before, value_after)`.
    #[inline]
    pub(crate) fn advance_reading(&mut self, now: Instant) -> (f32, f32) {
        let (decayed, gap) = match self.at {
            Some(at) => {
                let dt = now.saturating_duration_since(at).as_secs_f32();
                (self.value_after(dt), dt)
            }
            None => (0.0, TYPING_MOMENTUM_CREDIT_CAP_S),
        };
        let credit = TYPING_MOMENTUM_RATE * gap.min(TYPING_MOMENTUM_CREDIT_CAP_S);
        let next = (decayed + credit).min(1.0);
        self.value = next;
        self.at = Some(now);
        (decayed, next)
    }

    /// Record one printable advance at `now`, crediting `RATE * min(gap, CREDIT_CAP_S)`.
    #[inline]
    pub fn advance(&mut self, now: Instant) {
        let _ = self.advance_reading(now);
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
        if !(threshold.is_finite() && threshold > 0.0) {
            return None;
        }
        let at = self.at?;
        if self.value <= threshold {
            return Some(at);
        }
        let elapsed = TYPING_MOMENTUM_TAU * crate::color_math::ln_pos_f32(self.value / threshold);
        let dur = std::time::Duration::try_from_secs_f32(elapsed.max(0.0)).ok()?;
        at.checked_add(dur)
    }

    /// Deadline when [`Self::value`] reads `<= threshold` (`0.0..=1.0`), including [`SNAP_ZERO`].
    #[must_use]
    #[inline]
    pub fn reading_low_crossing(&self, threshold: f32) -> Option<Instant> {
        debug_assert!(threshold.is_finite() && (0.0..=1.0).contains(&threshold));
        if !(threshold.is_finite() && (0.0..=1.0).contains(&threshold)) {
            return None;
        }
        let at = self.at?;
        if self.value < SNAP_ZERO || self.value <= threshold {
            return Some(at);
        }
        let elapsed = TYPING_MOMENTUM_TAU
            * crate::color_math::ln_pos_f32(self.value / threshold.max(SNAP_ZERO));
        let dur = std::time::Duration::try_from_secs_f32(elapsed.max(0.0)).ok()?;
        at.checked_add(dur)?
            .checked_add(std::time::Duration::from_millis(1))
    }
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "typing_momentum_tests.rs"]
mod tests;
