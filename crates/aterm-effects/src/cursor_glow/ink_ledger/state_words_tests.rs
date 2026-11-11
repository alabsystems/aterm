// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::Instant;

fn words(l: &InkLedger, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    l.state_words(&mut w);
    w.into_words()
}

fn products(l: &InkLedger, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    l.ledger_words(&mut w);
    w.into_words()
}

#[test]
fn a_hashed_ink_ledger_field_moves_the_ledger_words() {
    let t0 = Instant::now();
    let mut ledger = InkLedger::EMPTY;
    ledger.note_rail(2, 5);
    let (state, base) = (words(&ledger, t0), products(&ledger, t0));
    assert_eq!(base, products(&ledger, t0));

    let mut railed = InkLedger::EMPTY;
    railed.note_rail(2, 6);
    let mut footed = InkLedger::EMPTY;
    footed.note_foot_rail(2, 5);
    let mut dimmed = InkLedger::EMPTY;
    dimmed.note_rail(2, 5);
    dimmed.dims.push(Dim {
        x0: 0.0,
        y0: 0.0,
        x1: 15.0,
        y1: 28.0,
        share: 0.5,
    });
    let mut noted = InkLedger::EMPTY;
    noted.note_rail(2, 5);
    noted.print ^= 1;
    for (what, l) in [
        ("a rail", &railed),
        ("a rail's foot mark", &footed),
        ("a dim", &dimmed),
        ("the cut notes", &noted),
    ] {
        assert_ne!(products(l, t0), base, "{what} is in the ledger's words");
        assert_eq!(words(l, t0), state, "{what} is not state");
    }
    let mut scratch = InkLedger::EMPTY;
    scratch.note_rail(2, 5);
    scratch.reached.push(3);
    assert_eq!(
        products(&scratch, t0),
        base,
        "the admit scratch is not hashed"
    );
    assert_eq!(words(&scratch, t0), state, "nor state");
    let mut moved = InkLedger::EMPTY;
    moved.note_rail(2, 5);
    moved.caret = Some((3, 4));
    assert_ne!(words(&moved, t0), state, "the account is state");
    assert_eq!(products(&moved, t0), base, "and not the ledger's");
}
