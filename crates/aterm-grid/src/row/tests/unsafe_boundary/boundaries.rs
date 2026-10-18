// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

// write_char_styled boundary tests

#[test]
fn write_char_styled_single_col_row() {
    let (_pages, mut row) = make_row(1);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    assert!(row.write_char_styled(0, 'A', fg, bg, CellFlags::empty()));
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert!(!row.write_char_styled(1, 'B', fg, bg, CellFlags::empty()));
}

// write_wide_char boundary tests

#[test]
fn write_wide_char_two_col_row() {
    let (_pages, mut row) = make_row(2);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    let ok = row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(ok);
    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());
}

#[test]
fn write_wide_char_single_col_row_rejected() {
    let (_pages, mut row) = make_row(1);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    let ok = row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(!ok);
}

#[test]
fn write_char_styled_all_columns_sequential() {
    // Write to every column in order — exercises the unsafe path at every index.
    let (_pages, mut row) = make_row(80);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    for col in 0..80u16 {
        assert!(row.write_char_styled(col, 'A', fg, bg, CellFlags::empty()));
    }
    assert_eq!(row.len(), 80);
    for col in 0..80u16 {
        assert_eq!(row.get(col).unwrap().char(), 'A');
    }
}
