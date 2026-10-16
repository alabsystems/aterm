// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

// Row's unsafe write paths stay sound when rows share a PageStore and when a
// resize reallocates (exercised under Miri).

/// Rows sharing a PageStore are independent: writing one does not alias or
/// invalidate another.
#[test]
fn multiple_rows_independent_writes() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: Test-local `pages` outlives all rows in the vector.
    let mut rows: Vec<Row> = (0..10)
        .map(|_| unsafe { Row::new(80, &mut pages) })
        .collect();

    for (row_idx, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + row_idx as u8);
        for col in 0..80u16 {
            assert!(row.write_char_styled(col, c, fg, bg, CellFlags::empty()));
        }
    }

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

/// Row resize copies content into a new PageSlice that is independently valid
/// (resize()'s alloc_slice + copy_from_slice path).
#[test]
fn resize_preserves_content_across_reallocation() {
    let (mut pages, mut row) = make_row(40);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    for col in 0..40u16 {
        row.write_char_styled(col, 'Z', fg, bg, CellFlags::BOLD);
    }

    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    unsafe { row.resize(80, &mut pages) };
    assert_eq!(row.cols(), 80);

    for col in 0..40u16 {
        let cell = row.get(col).unwrap();
        assert_eq!(cell.char(), 'Z', "col {col} char lost after resize");
        assert!(
            cell.flags().contains(CellFlags::BOLD),
            "col {col} BOLD flag lost after resize"
        );
    }

    for col in 40..80u16 {
        assert!(
            row.get(col).unwrap().is_empty(),
            "col {col} should be empty"
        );
    }
}

/// Wide char writes (unchecked access at col and col+1) at every valid
/// position, including col 0 and the last.
#[test]
fn write_wide_char_every_valid_position() {
    let (_pages, mut row) = make_row(20);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

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

    row.clear();
    let ok = row.write_wide_char(19, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(!ok, "wide char at last col should be rejected");
}
