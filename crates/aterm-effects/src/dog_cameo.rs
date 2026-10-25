// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed-word dog cameo lifecycle, seeded breed/coat roll, and entry bounce.

use std::time::Duration;

use aterm_time::Instant;

use crate::dog_glyphs_gen::{DOG_HEADS, DogGlyphId};
use crate::genome::mix;

const FADE_IN: Duration = Duration::from_millis(140);
const HOLD: Duration = Duration::from_millis(2600);
const FADE_OUT: Duration = Duration::from_millis(420);
const FADE_IN_HOLD: Duration = Duration::from_millis(140 + 2600);
const VISIT_LIFE: Duration = Duration::from_millis(140 + 2600 + 420);
const _: () = assert!(FADE_IN_HOLD.as_nanos() == FADE_IN.as_nanos() + HOLD.as_nanos());
const _: () = assert!(VISIT_LIFE.as_nanos() == FADE_IN_HOLD.as_nanos() + FADE_OUT.as_nanos());

const HOP_MS: u64 = 420;
const HOPS: u64 = 2;
const HOP_LIFT: f32 = 0.22;

const BREED_SALT: u64 = 0x646f_6721_6272_6564; // b"dog!bred"
const COAT_SALT: u64 = 0x646f_6721_636f_6174; // b"dog!coat"

/// Reduce a mixed seed onto `[0, upper)` with multiply-high (constant-time,
/// rejection-free, bias at most one part in 2^64).
#[inline]
const fn scale(entropy: u64, upper: usize) -> usize {
    ((entropy as u128 * upper as u128) >> 64) as usize
}

/// One visit's rolled identity: breed head and coat ramp index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DogLook {
    /// The authored breed head.
    pub breed: DogGlyphId,
    /// Coat ramp index (`0..=15`).
    pub coat: u8,
}

/// Per-window dog-cameo lifecycle state; `Default` is "never summoned".
pub struct DogCameo {
    /// The current visit's entry instant (`None` before the first summon; the accessors
    /// self-expire).
    since: Option<Instant>,
    /// The current visit's rolled identity.
    look: Option<DogLook>,
}

impl Default for DogCameo {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl DogCameo {
    /// Construct a fresh dog-cameo state before any summon.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            since: None,
            look: None,
        }
    }

    /// Summon a dog by rolling a breed and coat from `seed` and restarting the envelope.
    #[inline]
    pub fn on_summon(&mut self, now: Instant, seed: u64) {
        if DOG_HEADS.is_empty() {
            return;
        }
        let resident = self.active(now).then_some(self.look).flatten();
        let breed = match resident {
            Some(prev) if DOG_HEADS.len() > 1 => {
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
    #[inline]
    #[must_use]
    pub fn active(&self, now: Instant) -> bool {
        self.since
            .is_some_and(|at| now.saturating_duration_since(at) < VISIT_LIFE)
    }

    /// The visit's presence envelope (`0..=255`) at `now`.
    #[inline]
    #[must_use]
    pub fn alpha(&self, now: Instant) -> u8 {
        let Some(at) = self.since else {
            return 0;
        };
        let t = now.saturating_duration_since(at);
        if t < FADE_IN {
            let f = t.as_secs_f32() / FADE_IN.as_secs_f32();
            (f * 255.0).round().clamp(0.0, 255.0) as u8
        } else if t < FADE_IN_HOLD {
            255
        } else if t < VISIT_LIFE {
            let f = (t - FADE_IN_HOLD).as_secs_f32() / FADE_OUT.as_secs_f32();
            ((1.0 - f) * 255.0).round().clamp(0.0, 255.0) as u8
        } else {
            0
        }
    }

    /// The entry bounce at `now` in cell fractions (`<= 0` is up).
    #[inline]
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
        let lift = HOP_LIFT / (1 << hop) as f32;
        -lift * (phase * std::f32::consts::PI).sin()
    }

    /// The current visit's rolled identity (`None` before the first summon).
    #[inline]
    #[must_use]
    pub const fn look(&self) -> Option<DogLook> {
        self.look
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

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
