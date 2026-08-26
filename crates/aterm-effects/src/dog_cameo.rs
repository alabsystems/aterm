// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed-word DOG cameo — the canine twin of the typed-"kitty" summon.
//!
//! Typing a canine word (`dog`, `puppy`, `perro`, `chiot`, `子犬`, …) after the
//! window has TYPED A LOT (the detector's keystroke gate, see
//! `aterm-gui`'s `kitty_summon` module) pops a dog up next to the cursor. Where
//! the kitty cameo is the cursor companion saying hello — one persistent
//! identity, never re-rolled — the dog cameo is a VISITOR: every summon rolls a
//! fresh breed from the whole authored roster
//! ([`crate::dog_glyphs_gen::DOG_HEADS`]) and a fresh coat, so repeated
//! summons show off the wide selection. A re-summon while a dog is still on
//! screen always rolls a DIFFERENT breed — "dog dog dog" is a parade, not a
//! stutter.
//!
//! This module is the pure lifecycle state machine: the envelope (fade in →
//! hold → fade out), the deterministic breed/coat roll, and the happy entry
//! bounce. It draws nothing — the host resolves `alpha`/`bob`/`look` each
//! frame and hands them to `word_decorations::dog_cameo`, which bakes the
//! breed through [`crate::dog_baker::DogBaker`] and stamps the sprite. Like
//! every input-path effect it is Source-agnostic and clockless-deterministic:
//! all decisions derive from the summon seed and the host's `now`.

use std::time::Duration;

use aterm_time::Instant;

use crate::dog_glyphs_gen::{DOG_HEADS, DogGlyphId};
use crate::genome::mix;

/// Entry fade — quick enough to read as "popped up".
const FADE_IN: Duration = Duration::from_millis(140);
/// How long the dog stays at full presence after its entry.
const HOLD: Duration = Duration::from_millis(2600);
/// Exit fade — a soft dissolve, mirroring the kitty hello's departure scale.
const FADE_OUT: Duration = Duration::from_millis(420);

/// Entry-bounce spec: two happy hops, each shorter and lower than the last,
/// finished well inside the hold so the dog SETTLES while fully visible.
const HOP_MS: u64 = 420;
const HOPS: u64 = 2;
/// First hop's peak lift, as a fraction of a cell (negative = up at draw time).
const HOP_LIFT: f32 = 0.22;

/// Seed salts for the two independent rolls. Distinct constants so the breed
/// and coat draws can never correlate through a shared `mix` input.
const BREED_SALT: u64 = 0x646f_6721_6272_6564; // b"dog!bred"
const COAT_SALT: u64 = 0x646f_6721_636f_6174; // b"dog!coat"

/// Reduce a mixed seed onto `[0, upper)` with multiply-high — constant-time,
/// rejection-free, discrepancy ≤ one ticket in 2^64 (the genome's head-spill
/// idiom at full width).
fn scale(entropy: u64, upper: usize) -> usize {
    ((u128::from(entropy) * upper as u128) >> 64) as usize
}

/// One visit's rolled identity: which authored breed, wearing which
/// [`COAT_RAMP`](crate::cat_baker::COAT_RAMP) stop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DogLook {
    /// The authored breed head.
    pub breed: DogGlyphId,
    /// Coat ramp index (`0..=15`).
    pub coat: u8,
}

/// Per-window dog-cameo lifecycle state. `Default` is "no dog, never
/// summoned"; the state machine is driven entirely by
/// [`on_summon`](Self::on_summon) and read by the per-frame accessors.
#[derive(Default)]
pub struct DogCameo {
    /// The current visit's entry instant; `None` once the visit expires (the
    /// accessors self-expire — no per-frame host bookkeeping).
    since: Option<Instant>,
    /// The current visit's rolled identity.
    look: Option<DogLook>,
}

impl DogCameo {
    /// Summon a dog: roll a breed + coat from `seed` and (re)start the
    /// envelope. While a dog is still visible, the roll EXCLUDES the resident
    /// breed, so back-to-back summons always parade distinct dogs. A no-op on
    /// an empty roster (a placeholder build cannot panic here).
    pub fn on_summon(&mut self, now: Instant, seed: u64) {
        if DOG_HEADS.is_empty() {
            return;
        }
        let resident = self.active(now).then_some(self.look).flatten();
        let breed = match resident {
            Some(prev) if DOG_HEADS.len() > 1 => {
                // Uniform over the roster minus the resident breed: draw from
                // `len - 1` tickets and skip over the resident's index.
                let prev_ix = DOG_HEADS.iter().position(|&b| b == prev.breed).unwrap_or(0);
                let draw = scale(mix(seed ^ BREED_SALT), DOG_HEADS.len() - 1);
                DOG_HEADS[if draw >= prev_ix { draw + 1 } else { draw }]
            }
            _ => DOG_HEADS[scale(mix(seed ^ BREED_SALT), DOG_HEADS.len())],
        };
        let coat = (mix(seed ^ COAT_SALT) % 16) as u8;
        self.look = Some(DogLook { breed, coat });
        self.since = Some(now);
    }

    /// Whether a visit is in progress (any nonzero-alpha phase).
    #[must_use]
    pub fn active(&self, now: Instant) -> bool {
        self.since
            .is_some_and(|at| now.saturating_duration_since(at) < FADE_IN + HOLD + FADE_OUT)
    }

    /// The visit's presence envelope at `now`: `0..=255` through fade-in →
    /// hold → fade-out, `0` before the first summon and after expiry.
    #[must_use]
    pub fn alpha(&self, now: Instant) -> u8 {
        let Some(at) = self.since else {
            return 0;
        };
        let t = now.saturating_duration_since(at);
        if t < FADE_IN {
            let f = t.as_secs_f32() / FADE_IN.as_secs_f32();
            (f * 255.0).round().clamp(0.0, 255.0) as u8
        } else if t < FADE_IN + HOLD {
            255
        } else if t < FADE_IN + HOLD + FADE_OUT {
            let f = (t - FADE_IN - HOLD).as_secs_f32() / FADE_OUT.as_secs_f32();
            ((1.0 - f) * 255.0).round().clamp(0.0, 255.0) as u8
        } else {
            0
        }
    }

    /// The entry bounce at `now`, in cell fractions (`<= 0`: up). Two damped
    /// hops, then flat for the rest of the visit. The host passes `0.0`
    /// straight through under reduced motion by simply not calling this.
    #[must_use]
    pub fn bob(&self, now: Instant) -> f32 {
        let Some(at) = self.since else {
            return 0.0;
        };
        let ms = now.saturating_duration_since(at).as_millis() as u64;
        if ms >= HOP_MS * HOPS {
            return 0.0;
        }
        let hop = ms / HOP_MS;
        let phase = (ms % HOP_MS) as f32 / HOP_MS as f32;
        // Each hop is a half-sine arc at half the previous hop's height.
        let lift = HOP_LIFT / (1 << hop) as f32;
        -lift * (phase * std::f32::consts::PI).sin()
    }

    /// The current visit's rolled identity (`None` before the first summon).
    /// Stays resolvable through the fade-out so a mid-fade frame never loses
    /// its sprite.
    #[must_use]
    pub fn look(&self) -> Option<DogLook> {
        self.look
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    /// The envelope: invisible before the summon, opaque through the hold,
    /// gone after the fade — and `active` agrees with `alpha` at every edge.
    #[test]
    fn envelope_rises_holds_and_expires() {
        let mut d = DogCameo::default();
        let t = t0();
        assert_eq!(d.alpha(t), 0, "no dog before the first summon");
        assert!(!d.active(t));
        d.on_summon(t, 7);
        assert!(d.active(t));
        assert_eq!(d.alpha(t + FADE_IN), 255, "fully present after the fade-in");
        assert_eq!(d.alpha(t + FADE_IN + HOLD - Duration::from_millis(1)), 255);
        let gone = t + FADE_IN + HOLD + FADE_OUT;
        assert_eq!(d.alpha(gone), 0, "expired after the fade-out");
        assert!(!d.active(gone));
        assert!(
            d.look().is_some(),
            "the identity survives expiry for a mid-fade frame's benefit"
        );
    }

    /// Every summon rolls inside the roster, and the roll is a pure function
    /// of the seed (clockless determinism).
    #[test]
    fn rolls_are_deterministic_and_in_range() {
        let t = t0();
        for seed in 0..64u64 {
            let mut a = DogCameo::default();
            let mut b = DogCameo::default();
            a.on_summon(t, seed);
            b.on_summon(t, seed);
            let (la, lb) = (a.look().unwrap(), b.look().unwrap());
            assert_eq!(la, lb, "same seed ⇒ same roll");
            assert!(DOG_HEADS.contains(&la.breed));
            assert!(la.coat < 16);
        }
    }

    /// A re-summon while the dog is visible parades a DIFFERENT breed; the
    /// wide selection is reachable (many seeds hit many breeds).
    #[test]
    fn resummons_parade_distinct_breeds() {
        let t = t0();
        let mut d = DogCameo::default();
        d.on_summon(t, 1);
        let first = d.look().unwrap().breed;
        for seed in 0..32u64 {
            let mut fresh = DogCameo::default();
            fresh.on_summon(t, 1);
            fresh.on_summon(t + Duration::from_millis(300), seed);
            assert_ne!(
                fresh.look().unwrap().breed,
                first,
                "a visible dog is never re-rolled onto itself (seed {seed})"
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..256u64 {
            let mut fresh = DogCameo::default();
            fresh.on_summon(t, seed);
            seen.insert(fresh.look().unwrap().breed as usize);
        }
        assert!(
            seen.len() >= DOG_HEADS.len().min(8),
            "256 seeds should reach most of the roster — hit {} of {}",
            seen.len(),
            DOG_HEADS.len()
        );
    }

    /// The bounce: lifts (negative) inside the hop window, settles to exactly
    /// 0 afterwards, and the second hop is lower than the first.
    #[test]
    fn entry_bounce_hops_then_settles() {
        let mut d = DogCameo::default();
        let t = t0();
        d.on_summon(t, 3);
        let peak1 = d.bob(t + Duration::from_millis(HOP_MS / 2));
        let peak2 = d.bob(t + Duration::from_millis(HOP_MS + HOP_MS / 2));
        assert!(peak1 < 0.0, "a hop lifts the dog");
        assert!(peak2 < 0.0 && peak2 > peak1, "the second hop is lower");
        assert_eq!(
            d.bob(t + Duration::from_millis(HOP_MS * HOPS + 1)),
            0.0,
            "settled after the hops"
        );
        assert_eq!(DogCameo::default().bob(t), 0.0, "no bounce before a summon");
    }
}
