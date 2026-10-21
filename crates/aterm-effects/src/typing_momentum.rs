// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The canonical typing-momentum metric: one leaky integrator in `[0, 1]` that
//! every rainbow-kitty "earned drama" consumer reads.
//!
//! [`crate::cursor_glow::CursorGlow`] and [`crate::kitty_cursor::CursorCat`]
//! each hold one instance fed by the same host keystream, so they read the same
//! number (pinned by `momentum_unifies_glow_and_cat_metrics`).
//!
//! * Only non-delete printable typing advances build it
//!   ([`TypingMomentum::advance`]); deletes, kills and navigation never do.
//! * Each advance credits `RATE × min(gap, CREDIT_CAP_S)`, `gap` being the time
//!   since the last stamp: momentum is time spent typing, not key count. A
//!   key-repeat flood and a human sprint build at the same
//!   [`TYPING_MOMENTUM_RATE`] per second, and a lone key after a long pause
//!   earns at most one credit (≈ 0.23).
//! * Exponential decay, τ = 2 s: an inter-word breath (0.4–0.8 s) keeps 67–82 %
//!   of an earned run, a 3–4 s pause drains a full metric below every visual
//!   onset. There is no idle floor.
//! * A backspace drains [`TYPING_MOMENTUM_DELETE_DRAIN`] (a kill chord twice
//!   that) and re-stamps the clock, spending the interrupted gap credit.
//!
//! With `RATE × τ = 1.3` the flood equilibrium clamps at 1.0; flat-out typing
//! crosses the cat's 0.65 band in ~1.4 s and reaches 1.0 in ~2.9 s, the arc
//! the thresholds in [`crate::kitty_cursor`] are tuned against.

use aterm_time::Instant;

/// Exponential decay time constant (seconds): prose breaths survive, thinking
/// pauses drain.
pub const TYPING_MOMENTUM_TAU: f32 = 2.0;

/// Maximum accrual per wall-clock second of sustained typing. With
/// [`TYPING_MOMENTUM_TAU`] this fixes the build curve
/// `v(t) = RATE·τ·(1 − e^(−t/τ))`, clamped at 1: ≈ 0.51 after one second,
/// 0.75 at ~1.7 s, 1.0 at ~2.9 s.
pub const TYPING_MOMENTUM_RATE: f32 = 0.65;

/// Per-advance credit window (seconds): one keystroke credits at most
/// `RATE × CREDIT_CAP_S`. Just above the fastest deliberate cadence
/// (~300 ms/key), so human rhythms earn the same per-second rate, while slow
/// pecking is out-run by the decay (a 1 s/key stroll settles near 0.58, below
/// the cat band).
pub const TYPING_MOMENTUM_CREDIT_CAP_S: f32 = 0.35;

/// Mild per-backspace drain (about half one advance credit): fixing a typo
/// keeps an earned run alive, a held delete un-earns it.
pub const TYPING_MOMENTUM_DELETE_DRAIN: f32 = 0.12;

/// Kill-chord drain: erasing a span un-earns like two deletes.
pub const TYPING_MOMENTUM_KILL_DRAIN: f32 = 2.0 * TYPING_MOMENTUM_DELETE_DRAIN;

/// Below this a lazily-decayed read snaps to exactly 0.0, so idle consumers
/// can disarm on a clean zero instead of chasing denormal residue forever.
const SNAP_ZERO: f32 = 0.000_5;

/// The canonical leaky integrator (the law is in the module doc). `Copy`, so
/// the style-crossfade ghost can carry a frozen copy.
#[derive(Clone, Copy, Debug, Default)]
pub struct TypingMomentum {
    /// Value at the `at` stamp (reads decay it lazily from there).
    value: f32,
    /// The last build/drain stamp; `None` ⇒ the metric is at rest (0).
    at: Option<Instant>,
}

impl TypingMomentum {
    /// The metric at `now`: the stored value decayed over the elapsed gap on
    /// [`TYPING_MOMENTUM_TAU`], snapping to exactly 0 once negligible.
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
            self.value * (-dt / TYPING_MOMENTUM_TAU).exp()
        };
        if v < SNAP_ZERO { 0.0 } else { v }
    }

    /// One non-delete printable typing advance (forward glyph echo, wrap,
    /// coalesced multi-glyph echo). Decays to `now`, then credits
    /// `RATE × min(gap, CREDIT_CAP_S)`.
    pub fn advance(&mut self, now: Instant) {
        let gap = self.at.map_or(TYPING_MOMENTUM_CREDIT_CAP_S, |at| {
            now.saturating_duration_since(at).as_secs_f32()
        });
        let credit = TYPING_MOMENTUM_RATE * gap.min(TYPING_MOMENTUM_CREDIT_CAP_S);
        self.value = (self.value(now) + credit).min(1.0);
        self.at = Some(now);
    }

    /// One backspace: never builds, mildly drains, and re-stamps the clock, so
    /// deletes cannot launder accrual time.
    pub fn delete(&mut self, now: Instant) {
        self.value = (self.value(now) - TYPING_MOMENTUM_DELETE_DRAIN).max(0.0);
        self.at = Some(now);
    }

    /// One kill chord (Ctrl-K/U/W, Alt-D, forward Delete, word-backspace):
    /// drains [`TYPING_MOMENTUM_KILL_DRAIN`].
    pub fn kill(&mut self, now: Instant) {
        self.value = (self.value(now) - TYPING_MOMENTUM_KILL_DRAIN).max(0.0);
        self.at = Some(now);
    }

    /// Pin the metric to `v` (clamped to `[0, 1]`) at `now`.
    pub fn set_value(&mut self, now: Instant, v: f32) {
        self.value = v.clamp(0.0, 1.0);
        self.at = Some(now);
    }

    /// Back to rest (0, unstamped).
    pub fn reset(&mut self) {
        self.value = 0.0;
        self.at = None;
    }

    /// The analytic instant the decaying metric crosses `threshold` (`> 0`):
    /// `stamp + τ·ln(v/threshold)`, or the stamp when already at/below it;
    /// `None` while unstamped. Correct even when frames arrive late.
    #[must_use]
    pub fn low_crossing(&self, threshold: f32) -> Option<Instant> {
        let at = self.at?;
        if self.value <= threshold {
            return Some(at);
        }
        let elapsed = TYPING_MOMENTUM_TAU * (self.value / threshold).ln();
        Some(at + std::time::Duration::from_secs_f32(elapsed.max(0.0)))
    }

    /// A deadline at which a fresh [`Self::value`] reads at/below `threshold`
    /// (finite, in `0..=1`). Unlike the unsnapped [`Self::low_crossing`], this
    /// includes zero: a floor below [`SNAP_ZERO`] is reached at the snap. One
    /// millisecond past the analytic boundary covers its strict `<` and f32
    /// rounding.
    #[must_use]
    pub fn reading_low_crossing(&self, threshold: f32) -> Option<Instant> {
        debug_assert!(threshold.is_finite() && (0.0..=1.0).contains(&threshold));
        let at = self.at?;
        if self.value(at) <= threshold {
            return Some(at);
        }
        self.low_crossing(threshold.max(SNAP_ZERO))
            .map(|crossing| crossing + std::time::Duration::from_millis(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The accrual cap: a 5 ms key-repeat flood and a 30 ms sprint over the same
    /// second land on the same metric, both under `RATE·τ·(1 − e^(−t/τ))`.
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

    /// A key landing after a long pause earns at most one capped credit: pause
    /// time is decay time, never banked accrual.
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
        // The pause leaves only decay residue (0.23·e⁻⁵ ≈ 0.0015) on top of
        // the one capped credit.
        assert!(
            after_pause <= cap + 0.01,
            "10 s of silence buys no banked credit: {after_pause}"
        );
    }

    /// `low_crossing` matches the decay law analytically (the cat's fade-out
    /// relies on it being frame-rate independent).
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
        // Already at/below the threshold ⇒ the stamp itself.
        m.set_value(t0, 0.1);
        assert_eq!(m.low_crossing(0.14), Some(t0));
        // Unstamped ⇒ no crossing to report.
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

    /// A delete flood walks an earned run to zero (never below), and the
    /// re-stamp spends the interrupted gap credit.
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
        // One advance right after re-builds only the 60 ms credit: the delete
        // spent the clock.
        t += Duration::from_millis(60);
        m.advance(t);
        assert!(
            m.value(t) <= TYPING_MOMENTUM_RATE * 0.06 + 1e-4,
            "the delete stamped the clock; no banked credit survives"
        );
    }
}
