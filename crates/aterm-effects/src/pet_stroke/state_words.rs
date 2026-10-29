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
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(s: &StrokeDetector, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        s.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_contact_its_travel_and_the_cooldown_move_the_stroke_words() {
        let t0 = Instant::now();
        let base = words(&StrokeDetector::new(), t0);
        let mut touched = StrokeDetector::new();
        assert!(
            !touched.sample(t0, Some((3.0, 1.0)), 2.0),
            "a first contact"
        );
        assert_ne!(words(&touched, t0), base, "the contact sample is hashed");
        let mut stroked = StrokeDetector::new();
        stroked.sample(t0, Some((3.0, 1.0)), 2.0);
        stroked.sample(t0 + Duration::from_millis(200), Some((3.5, 1.0)), 2.0);
        assert!(stroked.travel > 0.0, "fixture: the move earned travel");
        let mut unstroked = StrokeDetector::new();
        unstroked.sample(t0, Some((3.0, 1.0)), 2.0);
        unstroked.last = stroked.last;
        assert_ne!(
            words(&stroked, t0),
            words(&unstroked, t0),
            "the travel is hashed"
        );
        let mut cooling = StrokeDetector::new();
        cooling.cooldown_until = Some(t0 + Duration::from_millis(1500));
        assert_ne!(words(&cooling, t0), base, "the cooldown is hashed");
        let later = t0 + Duration::from_secs(5);
        let mut shifted = StrokeDetector::new();
        shifted.sample(later, Some((3.0, 1.0)), 2.0);
        assert_eq!(
            words(&shifted, later),
            words(&touched, t0),
            "the sample's stamp is read against the script's start"
        );
    }
}
