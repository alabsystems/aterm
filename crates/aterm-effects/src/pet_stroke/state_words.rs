// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pointer-stroke detector's state words (`crate::state_words`): the last contact sample,
//! the travel it has earned toward a stroke and the cooldown after one.

use super::StrokeDetector;
use crate::state_words::Words;

impl StrokeDetector {
    /// The sample, the travel and the cooldown.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.opt("last", self.last, |f, s| {
            f.f32("x", s.point.0);
            f.f32("y", s.point.1);
            f.at("moved_at", s.moved_at);
        });
        w.f32("travel", self.travel);
        w.at_opt("cooldown_until", self.cooldown_until);
    }
}

#[cfg(test)]
#[path = "state_words_tests.rs"]
mod tests;
