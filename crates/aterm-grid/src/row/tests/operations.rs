// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// Row operation tests: selective_clear, line_size, copy_from,
// clear_range boundaries, update_len, resize edge cases,
// and cross-page memory safety.

use super::super::*;
use super::make_row;

// selective_clear and selective_clear_range

#[test]
fn selective_clear_range_skips_protected_cells() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write normal cells
    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_char_styled(1, 'B', fg, bg, CellFlags::empty());
    // Write a protected cell
    row.write_char_styled(2, 'P', fg, bg, CellFlags::PROTECTED);
    row.write_char_styled(3, 'D', fg, bg, CellFlags::empty());

    row.selective_clear_range(0, 4);

    // Unprotected cells should be cleared
    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert_eq!(row.get(1).unwrap().char(), ' ');
    assert_eq!(row.get(3).unwrap().char(), ' ');

    // Protected cell should remain
    assert_eq!(row.get(2).unwrap().char(), 'P');
}

#[test]
fn selective_clear_skips_protected_cells() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(5);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_char_styled(1, 'P', fg, bg, CellFlags::PROTECTED);
    row.write_char_styled(2, 'C', fg, bg, CellFlags::empty());

    row.selective_clear();

    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert_eq!(row.get(1).unwrap().char(), 'P');
    assert_eq!(row.get(2).unwrap().char(), ' ');
}

#[test]
fn selective_clear_range_empty_range() {
    let (_pages, mut row) = make_row(10);
    row.write_char(0, 'A');

    // Empty range — should be a no-op
    row.selective_clear_range(5, 5);
    assert_eq!(row.get(0).unwrap().char(), 'A');
}

// line_size / set_line_size

#[test]
fn line_size_default_is_single_width() {
    let (_pages, row) = make_row(80);
    assert_eq!(row.line_size(), LineSize::SingleWidth);
}

#[test]
fn set_line_size_double_width() {
    let (_pages, mut row) = make_row(80);
    for i in 0..80 {
        row.write_char(i, 'X');
    }

    row.set_line_size(LineSize::DoubleWidth);
    assert_eq!(row.line_size(), LineSize::DoubleWidth);
    assert!(row.flags().contains(RowFlags::DOUBLE_WIDTH));

    // Cells in the second half should be cleared
    assert_eq!(row.get(39).unwrap().char(), 'X');
    assert_eq!(row.get(40).unwrap().char(), ' ');
}

#[test]
fn set_line_size_double_height_top() {
    let (_pages, mut row) = make_row(80);
    row.set_line_size(LineSize::DoubleHeightTop);
    assert_eq!(row.line_size(), LineSize::DoubleHeightTop);
    assert!(row.flags().contains(RowFlags::DOUBLE_WIDTH));
    assert!(row.flags().contains(RowFlags::DOUBLE_HEIGHT_TOP));
}

#[test]
fn set_line_size_double_height_bottom() {
    let (_pages, mut row) = make_row(80);
    row.set_line_size(LineSize::DoubleHeightBottom);
    assert_eq!(row.line_size(), LineSize::DoubleHeightBottom);
    assert!(row.flags().contains(RowFlags::DOUBLE_WIDTH));
    assert!(row.flags().contains(RowFlags::DOUBLE_HEIGHT_BOTTOM));
}

#[test]
fn set_line_size_back_to_single_width() {
    let (_pages, mut row) = make_row(80);
    row.set_line_size(LineSize::DoubleWidth);
    row.set_line_size(LineSize::SingleWidth);
    assert_eq!(row.line_size(), LineSize::SingleWidth);
    assert!(!row.flags().contains(RowFlags::DOUBLE_WIDTH));
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

    // Only first 10 chars should be copied
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

    // End past row width — should clamp
    row.clear_range(8, 100);
    assert_eq!(row.get(7).unwrap().char(), 'H');
    assert_eq!(row.get(8).unwrap().char(), ' ');
    assert_eq!(row.get(9).unwrap().char(), ' ');
    assert_eq!(row.len(), 8);
}

#[test]
fn clear_range_no_op_when_start_equals_end() {
    let (_pages, mut row) = make_row(10);
    row.write_char(5, 'X');

    row.clear_range(5, 5);
    assert_eq!(row.get(5).unwrap().char(), 'X');
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

    // Attempting to set len beyond column count should clamp to cols()
    row.update_len(15);
    assert_eq!(row.len(), 10, "update_len must clamp to cols()");

    // u16::MAX should also clamp
    row.update_len(u16::MAX);
    assert_eq!(row.len(), 10, "update_len must clamp u16::MAX to cols()");
}

// resize edge cases

#[test]
fn resize_wide_char_at_boundary_is_cleared() {
    use crate::{CellFlags, PackedColor};

    let (mut pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Wide char at cols 8-9
    row.write_wide_char(8, '\u{4E2D}', fg, bg, CellFlags::empty());

    // Shrink to 9 cols — the wide char at col 8 can't display its second cell
    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    unsafe { row.resize(9, &mut pages) };

    // Col 8 should be cleared since the continuation cell at col 9 is gone
    assert_eq!(row.get(8).unwrap().char(), ' ');
    assert!(!row.get(8).unwrap().is_wide());
}

#[test]
fn resize_same_size_is_noop() {
    let (mut pages, mut row) = make_row(10);
    row.write_char(0, 'A');

    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    unsafe { row.resize(10, &mut pages) };
    assert_eq!(row.cols(), 10);
    assert_eq!(row.get(0).unwrap().char(), 'A');
}

// Memory safety: Row operations across page boundaries (MIRI-exercised)

/// Multiple rows sharing a PageStore: writes to one row must not corrupt another.
#[test]
fn multiple_rows_independent_writes() {
    use crate::{CellFlags, PackedColor};

    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Allocate 10 rows from the same PageStore
    // SAFETY: Test-local `pages` outlives all rows in the vector.
    let mut rows: Vec<Row> = (0..10)
        .map(|_| unsafe { Row::new(80, &mut pages) })
        .collect();

    // Write distinct content to each row using the unsafe write_char_styled path
    for (row_idx, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + row_idx as u8);
        for col in 0..80u16 {
            assert!(row.write_char_styled(col, c, fg, bg, CellFlags::empty()));
        }
    }

    // Verify each row's content is intact (no cross-row corruption)
    for (row_idx, row) in rows.iter().enumerate() {
        let expected = char::from(b'A' + row_idx as u8);
        for col in 0..80u16 {
            let actual = row.get(col).unwrap().char();
            assert_eq!(
                actual, expected,
                "row[{row_idx}][{col}] = '{actual}', expected '{expected}'"
            );
        }
    }
}

/// Row resize preserves content across reallocation.
#[test]
fn resize_preserves_content_across_reallocation() {
    use crate::{CellFlags, PackedColor};

    let (mut pages, mut row) = make_row(40);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Fill with styled content
    for col in 0..40u16 {
        row.write_char_styled(col, 'Z', fg, bg, CellFlags::BOLD);
    }

    // Resize to larger — allocates new PageSlice, copies content
    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    unsafe { row.resize(80, &mut pages) };
    assert_eq!(row.cols(), 80);

    // Verify original content survived the reallocation
    for col in 0..40u16 {
        let cell = row.get(col).unwrap();
        assert_eq!(cell.char(), 'Z', "col {col} char lost after resize");
        assert!(
            cell.flags().contains(CellFlags::BOLD),
            "col {col} BOLD flag lost after resize"
        );
    }

    // Verify new cells are empty
    for col in 40..80u16 {
        assert!(
            row.get(col).unwrap().is_empty(),
            "col {col} should be empty"
        );
    }
}

/// Wide char writes at every valid position in a row.
#[test]
fn write_wide_char_every_valid_position() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(20);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write a wide char at every valid starting position (0..=18)
    // Each write overwrites the previous, but we verify each one succeeds
    for col in 0..19u16 {
        row.clear();
        let ok = row.write_wide_char(col, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert!(ok, "wide char at col {col} should succeed");
        assert!(row.get(col).unwrap().is_wide(), "col {col} should be WIDE");
        assert!(
            row.get(col + 1).unwrap().is_wide_continuation(),
            "col {} should be WIDE_CONTINUATION",
            col + 1
        );
    }

    // Col 19 (last col) should be rejected
    row.clear();
    let ok = row.write_wide_char(19, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(!ok, "wide char at last col should be rejected");
}
