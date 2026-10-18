// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// Row operation tests: selective_clear, line_size, copy_from,
// clear_range boundaries and update_len.

use super::super::*;
use super::make_row;

// selective_clear and selective_clear_range

#[test]
fn selective_clear_skips_protected_cells() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(5);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_char_styled(1, 'P', fg, bg, CellFlags::PROTECTED);
    row.write_char_styled(2, 'C', fg, bg, CellFlags::empty());

    row.selective_clear(Cell::EMPTY);

    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert_eq!(row.get(1).unwrap().char(), 'P');
    assert_eq!(row.get(2).unwrap().char(), ' ');
}

// line_size / set_line_size

#[test]
fn set_line_size_double_width() {
    let (_pages, mut row) = make_row(80);
    for i in 0..80 {
        row.write_char(i, 'X');
    }

    row.set_line_size(LineSize::DoubleWidth);
    assert_eq!(row.line_size(), LineSize::DoubleWidth);
    assert!(row.flags().contains(RowFlags::DOUBLE_WIDTH));

    assert_eq!(row.get(39).unwrap().char(), 'X');
    assert_eq!(row.get(40).unwrap().char(), ' ');
}

/// Each line size maps onto its row flags, and SingleWidth clears them again.
#[test]
fn set_line_size_maps_onto_row_flags() {
    for (size, flags) in [
        (LineSize::DoubleHeightTop, RowFlags::DOUBLE_HEIGHT_TOP),
        (LineSize::DoubleHeightBottom, RowFlags::DOUBLE_HEIGHT_BOTTOM),
    ] {
        let (_pages, mut row) = make_row(80);
        row.set_line_size(size);
        assert_eq!(row.line_size(), size);
        assert!(row.flags().contains(RowFlags::DOUBLE_WIDTH), "{size:?}");
        assert!(row.flags().contains(flags), "{size:?}");
        row.set_line_size(LineSize::SingleWidth);
        assert_eq!(row.line_size(), LineSize::SingleWidth);
        assert!(!row.flags().contains(RowFlags::DOUBLE_WIDTH), "{size:?}");
        assert!(!row.flags().contains(flags), "{size:?}");
    }
}

// copy_from

#[test]
fn copy_from_same_width() {
    let (mut pages, mut row_a) = make_row(10);
    // SAFETY: Test-local `pages` outlives both rows for the full scope.
    let mut row_b = unsafe { Row::new(10, &mut pages) };

    for (i, c) in "Hello".chars().enumerate() {
        row_a.write_char(u16_from_usize(i), c);
    }
    row_a.set_wrapped(true);

    row_b.copy_from(&row_a);

    assert_eq!(row_b.to_string(), "Hello");
    assert_eq!(row_b.len(), 5);
    assert!(row_b.is_wrapped());
    assert!(row_b.is_dirty());
}

#[test]
fn copy_from_wider_source_clamps() {
    let (mut pages, mut row_wide) = make_row(20);
    // SAFETY: Test-local `pages` outlives both rows for the full scope.
    let mut row_narrow = unsafe { Row::new(10, &mut pages) };

    for (i, c) in "ABCDEFGHIJKLMNOPQRST".chars().enumerate() {
        row_wide.write_char(u16_from_usize(i), c);
    }

    row_narrow.copy_from(&row_wide);

    assert_eq!(row_narrow.to_string(), "ABCDEFGHIJ");
    assert_eq!(row_narrow.len(), 10);
}

// clear_range boundary cases

#[test]
fn clear_range_clamps_end_to_cols() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    row.clear_range_with(8, 100, Cell::EMPTY);
    assert_eq!(row.get(7).unwrap().char(), 'H');
    assert_eq!(row.get(8).unwrap().char(), ' ');
    assert_eq!(row.get(9).unwrap().char(), ' ');
    assert_eq!(row.len(), 8);
}

// update_len

#[test]
fn update_len_extends_but_never_shrinks() {
    let (_pages, mut row) = make_row(10);
    assert_eq!(row.len(), 0);

    row.update_len(5);
    assert_eq!(row.len(), 5);

    row.update_len(3);
    assert_eq!(row.len(), 5); // Should NOT shrink

    row.update_len(8);
    assert_eq!(row.len(), 8);
}

#[test]
fn update_len_clamps_to_cols() {
    let (_pages, mut row) = make_row(10);
    assert_eq!(row.len(), 0);

    row.update_len(15);
    assert_eq!(row.len(), 10, "update_len must clamp to cols()");

    row.update_len(u16::MAX);
    assert_eq!(row.len(), 10, "update_len must clamp u16::MAX to cols()");
}
