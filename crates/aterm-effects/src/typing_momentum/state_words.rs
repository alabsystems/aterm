// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The momentum integrator's state words (`crate::state_words`).

use super::TypingMomentum;
use crate::state_words::Words;

impl TypingMomentum {
    /// The stored value and its stamp.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.f32("value", self.value);
        w.at_opt("at", self.at);
    }
}
