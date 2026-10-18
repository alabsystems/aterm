// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

// Wide char overwrite interactions

#[test]
fn write_char_styled_overwrite_wide_first_half() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());

    row.write_char_styled(4, 'X', fg, bg, CellFlags::empty());

    assert_eq!(row.get(4).unwrap().char(), 'X');
    assert!(!row.get(4).unwrap().is_wide());
    assert_eq!(row.get(5).unwrap().char(), ' ');
    assert!(!row.get(5).unwrap().is_wide_continuation());
}

#[test]
fn write_wide_char_adjacent_pair() {
    // Overwrite the middle of three adjacent wide chars: both neighbours wide.
    let (_pages, mut row) = make_row(6);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E00}', fg, bg, CellFlags::empty()); // cols 0-1
    row.write_wide_char(2, '\u{4E8C}', fg, bg, CellFlags::empty()); // cols 2-3
    row.write_wide_char(4, '\u{4E09}', fg, bg, CellFlags::empty()); // cols 4-5

    row.write_wide_char(2, '\u{56DB}', fg, bg, CellFlags::empty());

    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());
    assert_eq!(row.get(2).unwrap().char(), '\u{56DB}');
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());
}
