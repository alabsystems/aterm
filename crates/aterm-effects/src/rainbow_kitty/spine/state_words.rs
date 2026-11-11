// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spine's state words (`crate::state_words`): the integrator, its follower and the flow run.

use super::{EraseMark, Spine};
use crate::state_words::Words;

impl Spine {
    /// Every field of the spine.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.group("spine.momentum", |f| self.momentum.state_words(f));
        w.f32("spine.disp", self.disp);
        w.f32("spine.disp_peak", self.disp_peak);
        w.f32("spine.phase", self.phase);
        w.at_opt("spine.at", self.at);
        w.flag("spine.slamming", self.slamming);
        w.opt("spine.mark", self.mark, |f, m: EraseMark| {
            f.at("at", m.at);
            f.f32("disp", m.disp);
            f.put("count", u64::from(m.count));
        });
        w.put("spine.run", u64::from(self.run));
        w.put("spine.best", u64::from(self.best));
        w.f32("spine.prev_birth", self.prev_birth);
        w.f32("spine.key_rise", self.key_rise);
        w.at_opt("spine.driven_at", self.driven_at);
        w.at_opt("spine.verdict", self.verdict);
    }
}

#[cfg(test)]
#[path = "state_words_tests.rs"]
mod tests;
