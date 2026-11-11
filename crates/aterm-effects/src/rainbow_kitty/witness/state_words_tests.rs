// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::Duration;

fn words(wit: &Witness, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    wit.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_witness_field_moves_the_witness_words() {
    let t0 = Instant::now();
    let mut wit = Witness::new();
    wit.seen.push(Seen {
        row: 4,
        col: 9,
        born: t0 + Duration::from_millis(12),
        unit: Unit {
            ch: 'a',
            wide: false,
            cont: false,
        },
        live: true,
        released: false,
        restorable: false,
        counted: true,
        clear: 0,
        twin: 0,
        pending: 0,
        pend_ms: [0; 4],
        deferred: false,
    });
    let base = words(&wit, t0);
    assert_eq!(base, words(&wit.clone(), t0));

    let mut released = wit.clone();
    released.seen[0].released = true;
    assert_ne!(
        words(&released, t0),
        base,
        "a Seen record's release is hashed"
    );
    let mut twinned = wit.clone();
    twinned.seen[0].pend_ms[2] = 40;
    assert_ne!(
        words(&twinned, t0),
        base,
        "a Seen record's twin clock is hashed"
    );
    let mut carried = wit.clone();
    carried.scroll_carry += 1;
    assert_ne!(words(&carried, t0), base, "the scroll carry is hashed");
    let mut armed = wit.clone();
    armed.follow_armed.push((9, Unit::BLANK, true, 1));
    assert_eq!(
        words(&armed, t0),
        base,
        "the follow pass's scratch is not state"
    );
}
