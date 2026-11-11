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
const FADE_IN_SECS: f32 = FADE_IN.as_secs_f32();
const FADE_OUT_SECS: f32 = FADE_OUT.as_secs_f32();
const INV_FADE_IN_SECS: f32 = 1.0 / FADE_IN_SECS;
const INV_FADE_OUT_SECS: f32 = 1.0 / FADE_OUT_SECS;
const _: () = assert!(FADE_IN_HOLD.as_nanos() == FADE_IN.as_nanos() + HOLD.as_nanos());
const _: () = assert!(VISIT_LIFE.as_nanos() == FADE_IN_HOLD.as_nanos() + FADE_OUT.as_nanos());

const HOP_MS: u64 = 420;
const HOPS: u64 = 2;
const HOP_LIFT: f32 = 0.22;
const HOP_LIFTS: [f32; HOPS as usize] = [HOP_LIFT, HOP_LIFT * 0.5];
const HOP_PHASE_RAD_PER_MS: f32 = std::f32::consts::PI / (HOP_MS as f32);

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
        let resident = if self.active(now) { self.look } else { None };
        let breed = match resident {
            Some(prev) if DOG_HEADS.len() > 1 => {
                let mut prev_ix = 0;
                let mut i = 0;
                while i < DOG_HEADS.len() {
                    if DOG_HEADS[i] as u8 == prev.breed as u8 {
                        prev_ix = i;
                        break;
                    }
                    i += 1;
                }
                let draw = scale(mix(seed ^ BREED_SALT), DOG_HEADS.len() - 1);
                DOG_HEADS[if draw >= prev_ix { draw + 1 } else { draw }]
            }
            _ => DOG_HEADS[scale(mix(seed ^ BREED_SALT), DOG_HEADS.len())],
        };
        let coat = (mix(seed ^ COAT_SALT) & 15) as u8;
        self.look = Some(DogLook { breed, coat });
        self.since = Some(now);
    }

    /// Whether a visit is in progress (any nonzero-alpha phase).
    #[inline]
    #[must_use]
    pub fn active(&self, now: Instant) -> bool {
        match self.since {
            Some(at) => now.saturating_duration_since(at).as_nanos() < VISIT_LIFE.as_nanos(),
            None => false,
        }
    }

    /// The visit's presence envelope (`0..=255`) at `now`.
    #[inline]
    #[must_use]
    pub fn alpha(&self, now: Instant) -> u8 {
        let Some(at) = self.since else {
            return 0;
        };
        let t = now.saturating_duration_since(at);
        let t_ns = t.as_nanos();
        if t_ns >= VISIT_LIFE.as_nanos() {
            0
        } else if t_ns < FADE_IN.as_nanos() {
            let f = t.as_secs_f32() * INV_FADE_IN_SECS;
            (f * 255.0).round().clamp(0.0, 255.0) as u8
        } else if t_ns < FADE_IN_HOLD.as_nanos() {
            255
        } else {
            let f = t.saturating_sub(FADE_IN_HOLD).as_secs_f32() * INV_FADE_OUT_SECS;
            ((1.0 - f) * 255.0).round().clamp(0.0, 255.0) as u8
        }
    }

    /// The entry bounce at `now` in cell fractions (`<= 0` is up).
    #[inline]
    #[must_use]
    pub fn bob(&self, now: Instant) -> f32 {
        let Some(at) = self.since else {
            return 0.0;
        };
        Self::bob_ms(now.saturating_duration_since(at).as_millis() as u64)
    }

    #[inline]
    const fn bob_ms(ms: u64) -> f32 {
        if ms >= HOP_MS * HOPS {
            return 0.0;
        }
        let hop = (ms / HOP_MS) as usize;
        let rem_ms = (ms % HOP_MS) as f32;
        let (sin_p, _) = crate::supernova::sin_cos_f32(rem_ms * HOP_PHASE_RAD_PER_MS);
        -HOP_LIFTS[hop] * sin_p
    }

    /// The current visit's rolled identity (`None` before the first summon).
    #[inline]
    #[must_use]
    pub const fn look(&self) -> Option<DogLook> {
        self.look
    }
}

#[cfg(test)]
#[path = "dog_cameo_tests.rs"]
mod tests;
