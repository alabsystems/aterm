// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE canonical typing-momentum metric — one leaky integrator in `[0, 1]`
//! that every rainbow kitty "earned drama" consumer reads (owner: "the kitty face is a
//! little too distracting and so are the stars. these should appear with more
//! momentum on the cursor motion … more carefully define that metric where it
//! decays over time and it builds up with non-delete typing activity
//! continuously").
//!
//! Before this module there were THREE parallel momentum trackers feeding the
//! Rainbow kitty family: the style-shared typing `heat` (a per-key cadence-credit
//! envelope, also slammed to 1.0 by jump flares), the cursor cat's private
//! `score` (0.34/key, τ 1.2 s), and the eased display spines derived from
//! each. They disagreed about what "momentum" meant — a key-repeat flood
//! maxed `heat` in ~8 keys, a jump flare lit the star field without a single
//! keystroke, and the cat's earn law was a key COUNT — which is exactly how
//! casual bursts kept buying the full fireworks. This module is the single
//! replacement law; [`crate::cursor_glow::CursorGlow`] (ribbon spine → stars,
//! fresh-ink pops, iridescence) and [`crate::kitty_cursor::CursorCat`] each hold
//! one instance fed by the same host keystream, so every consumer reads the same
//! number: `momentum_unifies_glow_and_cat_metrics` (cursor_glow tests) pins
//! that the two instances evolve identically under one key script.
//!
//! ## The law
//!
//! * **Builds ONLY from non-delete printable typing advances.** A forward
//!   glyph echo calls [`TypingMomentum::advance`]; deletes, kills, and
//!   navigation never call it, so no amount of erasing or scrubbing reads as
//!   "typing momentum".
//! * **Rate-normalized accrual.** Each advance credits
//!   `RATE × min(gap, CREDIT_CAP_S)` where `gap` is the time since the last
//!   stamp: momentum is earned by TIME SPENT TYPING, not by key count. A
//!   30 ms key-repeat flood and a 120 ms human sprint both build at exactly
//!   [`TYPING_MOMENTUM_RATE`] per wall-clock second — key-repeat cannot
//!   outrun real typing (the accrual cap), and a lone key after a long pause
//!   earns at most one full credit (`RATE × CREDIT_CAP_S ≈ 0.23`), so credit
//!   can never be banked across a pause.
//! * **Exponential decay, τ = 2 s.** Chosen so an ordinary inter-word breath
//!   (0.4–0.8 s) keeps 67–82 % of an earned run — real prose rhythm sustains
//!   the metric — while a 3–4 s thinking pause drains even a full metric
//!   below every visual onset. Pauses ONLY decay it: there is no idle floor.
//! * **Deletes never build; they mildly drain.** A backspace subtracts
//!   [`TYPING_MOMENTUM_DELETE_DRAIN`] (a kill chord twice that): one typo
//!   correction dents an earned run without erasing it, a delete flood walks
//!   it to zero. The drain also re-stamps the clock, so the gap credit a
//!   delete interrupts is spent, not saved.
//!
//! With `RATE × τ = 1.3` the flood equilibrium clamps at 1.0, and flat-out
//! typing crosses the cat's 0.75 band in ~1.7 s and reaches 1.0 in ~2.9 s —
//! the arc the raised thresholds in [`crate::kitty_cursor`] are tuned against.

use aterm_time::Instant;

/// Exponential decay time constant (seconds). See the module doc: prose
/// breaths survive, thinking pauses drain — "it decays over time".
pub const TYPING_MOMENTUM_TAU: f32 = 2.0;

/// Maximum accrual per wall-clock second of sustained typing. With
/// [`TYPING_MOMENTUM_TAU`] this fixes the whole build curve:
/// `v(t) = RATE·τ·(1 − e^(−t/τ))`, clamped at 1 — 0.45 after one second
/// (casual-burst territory stays low), 0.75 at ~1.7 s, 1.0 at ~2.9 s.
pub const TYPING_MOMENTUM_RATE: f32 = 0.65;

/// Per-advance credit window (seconds): one keystroke may credit at most
/// `RATE × CREDIT_CAP_S` no matter how long the pause before it. Sized just
/// above the fastest deliberate typing cadence (~300 ms/key) so every human
/// rhythm from sprint to stroll earns the same per-second rate, while slower
/// pecking (gaps ≥ the cap) earns fixed per-key credit that the τ-decay
/// between keys progressively out-runs — a 1 s/key stroll equilibrates near
/// 0.58, below the cat band, forever.
pub const TYPING_MOMENTUM_CREDIT_CAP_S: f32 = 0.35;

/// Mild per-backspace drain. Deletes NEVER build; this drain is deliberately
/// small (about half of one full advance credit) so fixing a typo mid-flow
/// keeps an earned run alive, while a held delete run visibly un-earns it.
pub const TYPING_MOMENTUM_DELETE_DRAIN: f32 = 0.12;

/// Kill-chord drain: erasing a SPAN un-earns like ~two deletes (the same
/// escalation the fire quench documents for Ctrl-K/U/W).
pub const TYPING_MOMENTUM_KILL_DRAIN: f32 = 2.0 * TYPING_MOMENTUM_DELETE_DRAIN;

/// Below this a lazily-decayed read snaps to exactly 0.0, so idle consumers
/// can disarm on a clean zero instead of chasing denormal residue forever.
const SNAP_ZERO: f32 = 0.000_5;

/// The canonical leaky integrator (see the module doc for the full law).
/// `Copy` on purpose: the style-crossfade ghost carries a frozen copy exactly
/// like the other thermal scalars.
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
        let Some(at) = self.at else {
            return 0.0;
        };
        let dt = now.saturating_duration_since(at).as_secs_f32();
        let v = self.value * (-dt / TYPING_MOMENTUM_TAU).exp();
        if v < SNAP_ZERO { 0.0 } else { v }
    }

    /// One NON-DELETE printable typing advance (a forward glyph echo, a wrap,
    /// a coalesced multi-glyph echo — anything that is letters arriving).
    /// Decays to `now`, then credits `RATE × min(gap, CREDIT_CAP_S)` — the
    /// rate normalization that makes key-repeat floods and human sprints
    /// build at the same per-second pace (module doc, "accrual cap").
    pub fn advance(&mut self, now: Instant) {
        let gap = self.at.map_or(TYPING_MOMENTUM_CREDIT_CAP_S, |at| {
            now.saturating_duration_since(at).as_secs_f32()
        });
        let credit = TYPING_MOMENTUM_RATE * gap.min(TYPING_MOMENTUM_CREDIT_CAP_S);
        self.value = (self.value(now) + credit).min(1.0);
        self.at = Some(now);
    }

    /// One backspace: never builds, mildly drains, and spends the pending gap
    /// credit (the stamp moves), so deletes cannot launder accrual time.
    pub fn delete(&mut self, now: Instant) {
        self.value = (self.value(now) - TYPING_MOMENTUM_DELETE_DRAIN).max(0.0);
        self.at = Some(now);
    }

    /// One kill chord (Ctrl-K/U/W, Alt-D, forward Delete, word-backspace):
    /// a span erased is ~two deletes of un-earning.
    pub fn kill(&mut self, now: Instant) {
        self.value = (self.value(now) - TYPING_MOMENTUM_KILL_DRAIN).max(0.0);
        self.at = Some(now);
    }

    /// Pin the metric to `v` at `now` — the collection hello's "guaranteed
    /// visible at full momentum" arm and the test fixtures' warm-start.
    pub fn set_value(&mut self, now: Instant, v: f32) {
        self.value = v.clamp(0.0, 1.0);
        self.at = Some(now);
    }

    /// Back to rest (0, unstamped) — layout/space resets only.
    pub fn reset(&mut self) {
        self.value = 0.0;
        self.at = None;
    }

    /// The analytic instant the decaying metric crosses `threshold`
    /// (`> 0`): `stamp + τ·ln(v/threshold)`, or the stamp itself when the
    /// stored value is already at/below it. `None` while unstamped. Lets a
    /// frame-rate-independent consumer (the cat's fade-out) recover the TRUE
    /// crossing even when frames arrive arbitrarily late.
    #[must_use]
    pub fn low_crossing(&self, threshold: f32) -> Option<Instant> {
        let at = self.at?;
        if self.value <= threshold {
            return Some(at);
        }
        let elapsed = TYPING_MOMENTUM_TAU * (self.value / threshold).ln();
        Some(at + std::time::Duration::from_secs_f32(elapsed.max(0.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// THE ACCRUAL CAP: a 5 ms key-repeat flood and a 30 ms sprint spanning
    /// the same wall-clock second land on the same metric (rate-normalized —
    /// momentum is time spent typing, not key count), and both stay under the
    /// analytic ceiling `RATE·τ·(1 − e^(−t/τ))`.
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

    /// NON-DELETE-ONLY: deletes and kills never raise the metric, from rest
    /// or from a warm run.
    #[test]
    fn deletes_and_kills_never_build() {
        let t0 = Instant::now();
        let mut cold = TypingMomentum::default();
        for i in 0..20u64 {
            cold.delete(t0 + Duration::from_millis(i * 30));
            cold.kill(t0 + Duration::from_millis(i * 30 + 15));
        }
        assert_eq!(
            cold.value(t0 + Duration::from_secs(1)),
            0.0,
            "a delete flood from rest builds nothing"
        );
        let mut warm = TypingMomentum::default();
        warm.set_value(t0, 0.8);
        let before = warm.value(t0 + Duration::from_millis(10));
        warm.delete(t0 + Duration::from_millis(10));
        let after = warm.value(t0 + Duration::from_millis(10));
        assert!(after < before, "a delete drains ({before} -> {after})");
        assert!(
            before - after <= TYPING_MOMENTUM_DELETE_DRAIN + 1e-4,
            "…and the drain is the documented MILD one"
        );
    }

    /// τ = 2 s: from a full metric, half is left after τ·ln 2 and e⁻² ≈ 13.5 %
    /// after 2τ — the documented "prose breaths survive, thinking pauses
    /// drain" curve.
    #[test]
    fn decay_follows_the_documented_tau() {
        let t0 = Instant::now();
        let mut m = TypingMomentum::default();
        m.set_value(t0, 1.0);
        let half = t0 + Duration::from_secs_f32(TYPING_MOMENTUM_TAU * 2.0_f32.ln());
        assert!((m.value(half) - 0.5).abs() < 0.01, "half-life at τ·ln2");
        let two_tau = t0 + Duration::from_secs_f32(2.0 * TYPING_MOMENTUM_TAU);
        assert!((m.value(two_tau) - (-2.0_f32).exp()).abs() < 0.01);
        // A word-breath (0.6 s) keeps ~74% — prose rhythm sustains a run.
        let breath = m.value(t0 + Duration::from_millis(600));
        assert!(
            breath > 0.72,
            "an inter-word breath keeps the run: {breath}"
        );
    }

    /// A key landing after a long pause earns at most ONE capped credit:
    /// pause time is decay time, never banked accrual.
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
        // the one capped credit — nothing banked.
        assert!(
            after_pause <= cap + 0.01,
            "10 s of silence buys no banked credit: {after_pause}"
        );
    }

    /// The lazy `low_crossing` matches the decay law analytically — the
    /// frame-rate-independence contract the cat's fade-out leans on.
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

    /// A delete flood walks an earned run to zero (never below), and the
    /// stamp motion means the interrupted gap credit is spent, not saved.
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
        // One advance right after re-builds only the tiny 60 ms credit — the
        // delete spent the clock.
        t += Duration::from_millis(60);
        m.advance(t);
        assert!(
            m.value(t) <= TYPING_MOMENTUM_RATE * 0.06 + 1e-4,
            "the delete stamped the clock; no banked credit survives"
        );
    }
}
