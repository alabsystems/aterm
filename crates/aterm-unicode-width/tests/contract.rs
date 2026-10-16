// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Upstream's per-character contract, and answers recorded from
//! unicode-width 0.2.2 before the switch where its tables and aterm's agree.

use unicode_width::UnicodeWidthChar;

#[test]
fn controls_are_none_and_printable_ascii_is_one() {
    for c in ('\0'..='\u{1F}').chain('\u{7F}'..='\u{9F}') {
        assert_eq!(c.width(), None, "U+{:04X} is a control", c as u32);
    }
    for c in ' '..='~' {
        assert_eq!(c.width(), Some(1), "{c:?}");
    }
}

#[test]
fn every_other_scalar_value_is_zero_one_or_two_columns() {
    let mut answered = 0;
    for c in '\u{A0}'..='\u{10FFFF}' {
        let w = c.width();
        assert!(matches!(w, Some(0..=2)), "U+{:04X} -> {w:?}", c as u32);
        answered += 1;
    }
    // The control: the range is the whole space above the C1 block.
    assert_eq!(answered, 1_111_904);
}

/// `(char, width)` as unicode-width 0.2.2 answered them, recorded 2026-10-15.
const RECORDED: &[(char, usize)] = &[
    ('\u{A0}', 1),
    ('\u{B1}', 1),
    ('\u{3B1}', 1),
    ('\u{5D0}', 1),
    ('\u{301}', 0),
    ('\u{E31}', 0),
    ('\u{200B}', 0),
    ('\u{200D}', 0),
    ('\u{2500}', 1),
    ('\u{4E2D}', 2),
    ('\u{D55C}', 2),
    ('\u{FF21}', 2),
    ('\u{1F600}', 2),
    ('\u{20000}', 2),
    ('\u{E0001}', 0),
    ('\u{10FFFD}', 1),
];

#[test]
fn the_recorded_characters_answer_as_upstream_did() {
    for &(c, want) in RECORDED {
        assert_eq!(c.width(), Some(want), "U+{:04X}", c as u32);
    }
}
