// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::super::PackedColor;
use super::super::super::cell::Cell;
use super::super::super::cell_flags::CellFlags;
use super::super::super::page::PageStore;
use super::super::{Row, RowFlags};

/// Shared test helper: create a Row with the given column count.
fn make_row(cols: u16) -> (PageStore, Row) {
    let mut pages = PageStore::new();
    // SAFETY: Test rows never outlive their local `pages` owner.
    let row = unsafe { Row::new(cols, &mut pages) };
    (pages, row)
}

fn default_colors() -> (PackedColor, PackedColor) {
    (PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG)
}

#[test]
fn write_char_out_of_bounds_returns_false() {
    let (_pages, mut row) = make_row(10);
    assert!(!row.write_char(10, 'X'));
    assert!(!row.write_char(100, 'X'));
    assert_eq!(row.len(), 0);
}

#[test]
fn write_char_overwrite_wide_continuation_clears_main() {
    let (_pages, mut row) = make_row(80);
    let (fg, bg) = default_colors();

    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    row.write_char(3, 'Y');
    assert_eq!(row.get(3).unwrap().char(), 'Y');
    assert_eq!(row.get(2).unwrap().char(), ' ');
    assert!(!row.get(2).unwrap().is_wide());
}

#[test]
fn write_char_packed_out_of_bounds() {
    use super::super::super::PackedColors;
    let (_pages, mut row) = make_row(5);
    let colors = PackedColors::DEFAULT;
    assert!(!row.write_char_packed(5, 'X', colors, CellFlags::empty()));
    assert!(!row.write_char_packed(100, 'X', colors, CellFlags::empty()));
    assert_eq!(row.len(), 0);
}

#[test]
fn write_wide_char_sets_has_wide_chars_flag() {
    let (_pages, mut row) = make_row(80);
    let (fg, bg) = default_colors();
    assert!(!row.flags().contains(RowFlags::HAS_WIDE_CHARS));
    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.flags().contains(RowFlags::HAS_WIDE_CHARS));
}

#[test]
fn write_wide_char_overwrite_existing_wide_orphan_cleanup() {
    let (_pages, mut row) = make_row(80);
    let (fg, bg) = default_colors();

    row.write_wide_char(2, '\u{4E00}', fg, bg, CellFlags::empty());
    row.write_wide_char(4, '\u{4E8C}', fg, bg, CellFlags::empty());

    row.write_wide_char(3, '\u{4E09}', fg, bg, CellFlags::empty());

    assert_eq!(row.get(2).unwrap().char(), ' ');
    assert!(row.get(3).unwrap().is_wide());
    assert!(row.get(4).unwrap().is_wide_continuation());
    assert_eq!(row.get(5).unwrap().char(), ' ');
}

#[test]
fn write_wide_char_packed_out_of_bounds() {
    use super::super::super::PackedColors;
    let (_pages, mut row) = make_row(5);
    let colors = PackedColors::DEFAULT;
    assert!(!row.write_wide_char_packed(4, '\u{4E2D}', colors, CellFlags::empty()));
    assert_eq!(row.len(), 0);
}

/// A run of printed default blanks is written text: `len` covers it as it
/// covers the same blanks written one at a time, so a splatted or REP'd run
/// of spaces is part of the line.
#[test]
fn fill_cell_run_counts_written_blanks() {
    let (_pages, mut row) = make_row(10);
    row.write_char(0, 'A');
    assert_eq!(row.len(), 1);

    let written = row.fill_cell_run(5, 3, Cell::EMPTY);
    assert_eq!(written, 3);
    assert_eq!(row.len(), 8, "the filled blanks are written");

    let (_pages, mut per_glyph) = make_row(10);
    per_glyph.write_char(0, 'A');
    for col in 5..8 {
        per_glyph.write_char(col, ' ');
    }
    assert_eq!(row.len(), per_glyph.len(), "the same as one at a time");
}

#[test]
fn write_char_extends_len_for_sparse_writes() {
    let (_pages, mut row) = make_row(80);
    row.write_char(50, 'A');
    assert_eq!(row.len(), 51);
    row.write_char(10, 'B');
    assert_eq!(row.len(), 51);
}
