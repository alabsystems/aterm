// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

// ========================================================================
// Wide char overwrite interactions
// ========================================================================

#[test]
fn write_char_styled_overwrite_wide_first_half() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write wide char at col 4-5
    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());

    // Overwrite the first half (col 4) with a narrow char
    row.write_char_styled(4, 'X', fg, bg, CellFlags::empty());

    // First half replaced, continuation should be cleared
    assert_eq!(row.get(4).unwrap().char(), 'X');
    assert!(!row.get(4).unwrap().is_wide());
    assert_eq!(row.get(5).unwrap().char(), ' ');
    assert!(!row.get(5).unwrap().is_wide_continuation());
}

#[test]
fn write_char_styled_overwrite_wide_second_half() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write wide char at col 4-5
    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());

    // Overwrite the continuation (col 5) with a narrow char
    row.write_char_styled(5, 'Y', fg, bg, CellFlags::empty());

    // Continuation replaced, first half should be cleared to space
    assert_eq!(row.get(4).unwrap().char(), ' ');
    assert!(!row.get(4).unwrap().is_wide());
    assert_eq!(row.get(5).unwrap().char(), 'Y');
}

#[test]
fn write_wide_char_overwrite_existing_wide_overlap() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write wide char at col 2-3
    row.write_wide_char(2, '\u{4E00}', fg, bg, CellFlags::empty());
    // Write wide char at col 3-4 (overlaps continuation of first)
    row.write_wide_char(3, '\u{4E8C}', fg, bg, CellFlags::empty());

    // Col 2 (was first half of first wide) should be cleared
    assert_eq!(row.get(2).unwrap().char(), ' ');
    assert!(!row.get(2).unwrap().is_wide());
    // Col 3-4 has new wide char
    assert!(row.get(3).unwrap().is_wide());
    assert_eq!(row.get(3).unwrap().char(), '\u{4E8C}');
    assert!(row.get(4).unwrap().is_wide_continuation());
}

#[test]
fn write_wide_char_adjacent_pair() {
    // Fill a row with adjacent wide chars, then overwrite the middle one.
    // This exercises the fixup code when both neighbors are wide.
    let (_pages, mut row) = make_row(6);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E00}', fg, bg, CellFlags::empty()); // cols 0-1
    row.write_wide_char(2, '\u{4E8C}', fg, bg, CellFlags::empty()); // cols 2-3
    row.write_wide_char(4, '\u{4E09}', fg, bg, CellFlags::empty()); // cols 4-5

    // Overwrite cols 2-3 with a new wide char
    row.write_wide_char(2, '\u{56DB}', fg, bg, CellFlags::empty());

    // Neighbors should be unaffected
    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());
    assert_eq!(row.get(2).unwrap().char(), '\u{56DB}');
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());
}
