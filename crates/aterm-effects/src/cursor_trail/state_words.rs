// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing cadence's state words (`crate::state_words`): its heat, the keystroke it was last
//! stamped at and the tunables it decays and ignites by.

use super::TypingCadence;
use crate::state_words::Words;

impl TypingCadence {
    /// The heat, its stamp and the tunables.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.f32("heat", self.heat);
        w.at_opt("last", self.last);
        w.f32("gain", self.params.gain);
        w.dur("half_life", self.params.half_life);
        w.f32("knee_lo", self.params.knee_lo);
        w.f32("knee_hi", self.params.knee_hi);
    }
}
