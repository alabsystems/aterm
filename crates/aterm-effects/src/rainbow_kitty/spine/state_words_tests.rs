// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
