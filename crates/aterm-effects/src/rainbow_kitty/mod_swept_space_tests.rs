// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn key(cells: u16, class: TypedClass) -> Event {
    Event::Typed {
        cells,
        shifted: false,
        class,
    }
}

fn batch(keys: &[Event], width: u16, licence: Licence) -> Vec<(Event, Instant)> {
    let now = Instant::now();
    let mut events: Vec<_> = keys.iter().map(|&event| (event, now)).collect();
    events.push((
        Event::Sweep {
            row: 4,
            col0: 40,
            col1: 40 + width,
        },
        now,
    ));
    events.push((
        Event::Move {
            from: (4, 40),
            to: (4, 40 + width),
            licence,
            dir: Dir::Right,
        },
        now,
    ));
    events
}

#[test]
fn a_swept_space_uses_the_last_space_and_counts_wide_glyph_cells() {
    let keys = [
        key(2, TypedClass::Glyph),
        key(1, TypedClass::Space),
        key(1, TypedClass::Glyph),
        key(1, TypedClass::Space),
        key(2, TypedClass::Glyph),
    ];
    let events = batch(&keys, 7, Licence::Typed);
    assert_eq!(swept_space_cell(&events, keys.len()), Some((4, 44)));
}

#[test]
fn a_swept_space_requires_the_complete_key_run_and_typed_move() {
    let keys = [key(1, TypedClass::Space), key(1, TypedClass::Glyph)];
    for licence in [
        Licence::Insert,
        Licence::Nav,
        Licence::Return,
        Licence::Pty,
        Licence::Synthetic,
    ] {
        assert_eq!(swept_space_cell(&batch(&keys, 2, licence), 2), None);
    }
    for width in [0, 1, 3] {
        assert_eq!(
            swept_space_cell(&batch(&keys, width, Licence::Typed), 2),
            None
        );
    }
    for malformed in [
        vec![key(0, TypedClass::Space), key(2, TypedClass::Glyph)],
        vec![key(2, TypedClass::Space)],
        vec![keys[0], Event::Erase, keys[1]],
    ] {
        assert_eq!(
            swept_space_cell(&batch(&malformed, 2, Licence::Typed), malformed.len()),
            None
        );
    }
    let mut wrong_move = batch(&keys, 2, Licence::Typed);
    wrong_move[3].0 = Event::Move {
        from: (4, 39),
        to: (4, 42),
        licence: Licence::Typed,
        dir: Dir::Right,
    };
    assert_eq!(swept_space_cell(&wrong_move, 2), None);
    wrong_move.pop();
    assert_eq!(swept_space_cell(&wrong_move, 2), None);
}
