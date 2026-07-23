// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

// ========================================================================
// write_char_styled boundary tests
// ========================================================================

#[test]
fn write_char_styled_first_col() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    assert!(row.write_char_styled(0, 'Z', fg, bg, CellFlags::empty()));
    assert_eq!(row.get(0).unwrap().char(), 'Z');
    assert_eq!(row.len(), 1);
}

#[test]
fn write_char_styled_last_col() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    assert!(row.write_char_styled(9, 'Z', fg, bg, CellFlags::empty()));
    assert_eq!(row.get(9).unwrap().char(), 'Z');
    assert_eq!(row.len(), 10);
}

#[test]
fn write_char_styled_out_of_bounds() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    assert!(!row.write_char_styled(10, 'X', fg, bg, CellFlags::empty()));
    assert!(!row.write_char_styled(100, 'Y', fg, bg, CellFlags::empty()));
    assert_eq!(row.len(), 0);
}

#[test]
fn write_char_styled_single_col_row() {
    let (_pages, mut row) = make_row(1);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    assert!(row.write_char_styled(0, 'A', fg, bg, CellFlags::empty()));
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert!(!row.write_char_styled(1, 'B', fg, bg, CellFlags::empty()));
}

// ========================================================================
// write_wide_char boundary tests
// ========================================================================

#[test]
fn write_wide_char_first_col() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    let ok = row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(ok);

    let cell0 = row.get(0).unwrap();
    let cell1 = row.get(1).unwrap();
    assert!(cell0.is_wide());
    assert_eq!(cell0.char(), '\u{4E2D}');
    assert!(cell1.is_wide_continuation());
}

#[test]
fn write_wide_char_last_valid_col() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Col 8 is the last position where a wide char fits (needs cols 8 and 9)
    let ok = row.write_wide_char(8, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(ok);
    assert!(row.get(8).unwrap().is_wide());
    assert!(row.get(9).unwrap().is_wide_continuation());
}

#[test]
fn write_wide_char_last_col_rejected() {
    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Col 9 only has room for 1 cell, wide char needs 2
    let ok = row.write_wide_char(9, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(!ok);
}

#[test]
fn write_wide_char_two_col_row() {
    // Minimum row that can hold a wide char
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
    // Row with only 1 col can never hold a wide char
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
