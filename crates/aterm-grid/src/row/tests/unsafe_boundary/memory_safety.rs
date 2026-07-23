// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

// ========================================================================
// Memory safety: Row operations across page boundaries (MIRI-exercised)
// ========================================================================
//
// These tests verify that Row's unsafe write paths remain sound when
// multiple rows share a PageStore and allocations span page boundaries.

/// Multiple rows sharing a PageStore: writes to one row must not corrupt another.
///
/// This exercises the core invariant that PageSlice pointers derived from
/// the same PageStore are independent — writing through one doesn't alias
/// or invalidate another.
#[test]
fn multiple_rows_independent_writes() {
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

/// Row resize reallocates cells into a new PageSlice. Verify the old
/// content is correctly copied and the new slice is independently valid.
///
/// This exercises resize()'s alloc_slice + copy_from_slice path, which
/// creates a new NonNull<Cell> pointer and abandons the old one.
#[test]
fn resize_preserves_content_across_reallocation() {
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
///
/// This exercises the two-cell unsafe path (get_unchecked at col and
/// col+1) at every valid boundary, including col 0 and the last valid
/// position. Catches off-by-one errors in bounds checks.
#[test]
fn write_wide_char_every_valid_position() {
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
