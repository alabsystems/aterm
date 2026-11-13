// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row_of(bytes: &[u8]) -> Terminal {
    let mut term = Terminal::new(2, 12);
    term.process(bytes);
    term
}

#[test]
fn the_key_row_and_the_frame_observe_agree_cell_by_cell() {
    let rows: [(&str, &[u8]); 6] = [
        ("plain", b"$ ab cd"),
        ("right prompt", b"$ ab\x1b[9G\x1b[3m<r>\x1b[23m"),
        ("wide glyph", "$ a日b".as_bytes()),
        ("combining", "$ e\u{301}x".as_bytes()),
        ("emoji cluster", "$ 👍🏽 ok".as_bytes()),
        ("right to left", "$ \u{5d0}\u{5d1} ab".as_bytes()),
    ];
    for (label, bytes) in rows {
        let mut term = row_of(bytes);
        let mut key = KeyRow::default();
        seen_row(&term, 0, &mut key);
        let mut frame = RenderInput::empty();
        term.cell_frame_into(&mut frame, 2, 12);
        let observed: Vec<Seen> = (0..12).map(|c| seen_at(&frame, 0, c)).collect();
        assert_eq!(key.cells(), observed.as_slice(), "{label}");
        let opaque = key
            .cells()
            .iter()
            .filter(|s| matches!(s, Seen::Opaque(_)))
            .count();
        let expected_opaque = match label {
            "plain" | "right prompt" => 0,
            "wide glyph" => 2,
            "combining" => 1,
            "emoji cluster" => 2,
            _ => 7,
        };
        assert_eq!(opaque, expected_opaque, "{label}: {:?}", key.cells());
    }
}

#[test]
fn the_key_row_reads_the_row_under_it_as_the_frame_does() {
    let mut term = row_of("$ ab\r\nPassword: \x1b[A\x1b[5G".as_bytes());
    let mut key = KeyRow::default();
    seen_row(&term, 0, &mut key);
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 12);
    let under: Vec<Seen> = (0..12).map(|c| seen_at(&frame, 1, c)).collect();
    assert_eq!(key.below(), Some(under.as_slice()));
    let top: Vec<Seen> = (0..12).map(|c| seen_at(&frame, 0, c)).collect();
    assert_eq!(
        key.cells(),
        top.as_slice(),
        "the cursor row is still the row read"
    );
    seen_row(&term, 1, &mut key);
    assert_eq!(key.below(), None, "no row under the last");
    assert_eq!(key.cells(), under.as_slice());
}

#[test]
fn a_mirrored_bracket_is_the_one_cell_they_disagree_on() {
    let mut term = row_of("$ \u{5d0}(\u{5d1}) x".as_bytes());
    let mut key = KeyRow::default();
    seen_row(&term, 0, &mut key);
    let mut frame = RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 12);
    let mirror = |s: Seen| match s {
        Seen::Opaque(Some('(')) => Seen::Opaque(Some(')')),
        Seen::Opaque(Some(')')) => Seen::Opaque(Some('(')),
        other => other,
    };
    let mut mirrored = 0;
    for c in 0..12u16 {
        let k = key
            .cells()
            .get(usize::from(c))
            .copied()
            .unwrap_or(Seen::Blank);
        let f = seen_at(&frame, 0, c);
        if k != f {
            assert_eq!(mirror(k), f, "column {c}: only a mirrored bracket differs");
            mirrored += 1;
        }
    }
    assert_eq!(
        mirrored,
        2,
        "both brackets, and nothing else: {:?}",
        key.cells()
    );
}

#[test]
fn an_opaque_cell_carries_the_plain_answer() {
    let term = row_of("$ a日b".as_bytes());
    let mut key = KeyRow::default();
    seen_row(&term, 0, &mut key);
    assert_eq!(key.cells().get(2), Some(&Seen::Glyph('a')));
    assert_eq!(key.cells().get(3), Some(&Seen::Opaque(Some('日'))));
    assert_eq!(key.cells().get(4), Some(&Seen::Opaque(None)));
    assert_eq!(key.cells().get(5), Some(&Seen::Glyph('b')));
    assert_eq!(key.cells().get(6), Some(&Seen::Blank));
}
