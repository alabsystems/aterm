// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::{Duration, Instant};

fn words(c: &CompanionOwner, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    c.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_companion_field_moves_the_companion_words() {
    let t0 = Instant::now();
    let base = words(&CompanionOwner::new(), t0);
    assert_eq!(base, words(&CompanionOwner::new(), t0));

    let mut hit = CompanionOwner::new();
    hit.hit_rect = Some((10, 20, 40, 48));
    assert_ne!(words(&hit, t0), base, "the hit rect is hashed");
    let mut shed = CompanionOwner::new();
    shed.last_shed_tick = Some(t0 + Duration::from_millis(250));
    assert_ne!(words(&shed, t0), base, "the shed tick's clock is hashed");
    let mut latched = CompanionOwner::new();
    latched.latches.cmd_seq = Some((1, 2));
    assert_ne!(words(&latched, t0), base, "a session latch is hashed");
    assert!(
        base.iter().any(|&(name, _)| name == "pet.col"),
        "the brain's words are part of the driver's"
    );
}
