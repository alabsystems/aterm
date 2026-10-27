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
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(s: &Spine, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        s.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_spine_field_moves_the_spine_words() {
        let t0 = Instant::now();
        let mut s = Spine::new();
        s.update(t0);
        s.advance(t0 + Duration::from_millis(10));
        s.update(t0 + Duration::from_millis(16));
        let base = words(&s, t0);
        assert_eq!(base, words(&s.clone(), t0));

        let mut peak = s;
        peak.disp_peak += 0.25;
        assert_ne!(words(&peak, t0), base, "the follower's peak is hashed");
        let mut stamped = s;
        stamped.verdict = Some(t0 + Duration::from_millis(3));
        assert_ne!(words(&stamped, t0), base, "the verdict's stamp is hashed");
        let mut momentum = s;
        momentum.momentum = crate::typing_momentum::TypingMomentum::new();
        assert_ne!(words(&momentum, t0), base, "the integrator is hashed");
    }
}
