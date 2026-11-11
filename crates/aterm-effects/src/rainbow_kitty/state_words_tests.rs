// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::Duration;

fn words(e: &Engine, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    e.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_engine_custody_field_moves_the_engine_words() {
    let t0 = Instant::now();
    let mut e = Engine::new();
    e.set_engaged(true);
    e.on_event(
        Event::Typed {
            cells: 1,
            shifted: false,
            class: super::super::TypedClass::Glyph,
        },
        t0 + Duration::from_millis(3),
    );
    let base = words(&e, t0);
    assert_eq!(base, words(&e.clone(), t0));

    let mut banked = e.clone();
    banked.echo.bank(t0 + Duration::from_millis(9), 2);
    assert_ne!(words(&banked, t0), base, "the echo ledger is hashed");
    let mut queued = e.clone();
    queued
        .events
        .push((Event::Erase, t0 + Duration::from_millis(9)));
    assert_ne!(words(&queued, t0), base, "a queued event is hashed");
    let mut withheld = e.clone();
    withheld.withheld[0] = 7;
    withheld.withheld_n = 1;
    assert_ne!(words(&withheld, t0), base, "a withheld row is hashed");
    let mut followed = e.clone();
    followed.followed += 1;
    assert_ne!(words(&followed, t0), base, "the follow count is hashed");
    let mut stale = e.clone();
    stale.withheld[3] = 9;
    stale.fp ^= 1;
    assert_eq!(
        words(&stale, t0),
        base,
        "rows past the withheld count and the frame's fingerprint are not state"
    );
}
