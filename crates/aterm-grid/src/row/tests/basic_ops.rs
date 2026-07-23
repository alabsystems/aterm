// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// Basic row operations: new, write, clear, resize, wrapped flag,
// insert/delete/erase chars, and wide-char overwrite.

use super::super::*;
use super::make_row;

#[test]
fn row_new() {
    let (_pages, row) = make_row(80);
    assert_eq!(row.cols(), 80);
    assert_eq!(row.len(), 0);
    assert!(row.is_empty());
    assert!(row.is_dirty());
}

#[test]
fn row_write_char() {
    let (_pages, mut row) = make_row(80);
    assert!(row.write_char(0, 'H'));
    assert!(row.write_char(1, 'i'));
    assert_eq!(row.len(), 2);
    assert_eq!(row.get(0).unwrap().char(), 'H');
    assert_eq!(row.get(1).unwrap().char(), 'i');
}

#[test]
fn row_clear() {
    let (_pages, mut row) = make_row(80);
    row.write_char(0, 'X');
    row.write_char(10, 'Y');
    assert_eq!(row.len(), 11);

    row.clear();
    assert_eq!(row.len(), 0);
    assert!(row.is_empty());
}

#[test]
fn row_clear_from() {
    let (_pages, mut row) = make_row(80);
    for i in 0..10 {
        row.write_char(i, 'X');
    }
    assert_eq!(row.len(), 10);

    row.clear_from(5);
    assert_eq!(row.len(), 5);
}

#[test]
fn row_clear_from_sparse_tail() {
    let (_pages, mut row) = make_row(10);
    row.write_char(9, 'Z');
    assert_eq!(row.len(), 10);

    row.clear_from(5);
    assert_eq!(row.len(), 0);
}

#[test]
fn row_resize_grow() {
    let (mut pages, mut row) = make_row(40);
    row.write_char(0, 'A');
    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    unsafe { row.resize(80, &mut pages) };
    assert_eq!(row.cols(), 80);
    assert_eq!(row.get(0).unwrap().char(), 'A');
}

#[test]
fn row_resize_shrink() {
    let (mut pages, mut row) = make_row(80);
    row.write_char(60, 'A');
    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    unsafe { row.resize(40, &mut pages) };
    assert_eq!(row.cols(), 40);
    // Cell at 60 is now gone
    assert!(
        row.get(60).is_none(),
        "column 60 should not exist after shrink to 40"
    );
}

#[test]
fn row_to_string() {
    let (_pages, mut row) = make_row(80);
    for (i, c) in "Hello".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.to_string(), "Hello");
}

#[test]
fn row_wrapped_flag() {
    let (_pages, mut row) = make_row(80);
    assert!(!row.is_wrapped());
    row.set_wrapped(true);
    assert!(row.is_wrapped());
    row.set_wrapped(false);
    assert!(!row.is_wrapped());
}

#[test]
fn row_insert_chars() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.to_string(), "ABCDEFGHIJ");

    // Insert 2 blanks at position 3
    row.insert_chars(3, 2);
    // "ABC  DEFGH" - IJ are pushed off the end
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');
    assert_eq!(row.get(2).unwrap().char(), 'C');
    assert_eq!(row.get(3).unwrap().char(), ' ');
    assert_eq!(row.get(4).unwrap().char(), ' ');
    assert_eq!(row.get(5).unwrap().char(), 'D');
    assert_eq!(row.get(6).unwrap().char(), 'E');
    assert_eq!(row.get(7).unwrap().char(), 'F');
}

#[test]
fn row_delete_chars() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.to_string(), "ABCDEFGHIJ");

    // Delete 2 chars at position 3 (D and E)
    row.delete_chars(3, 2);
    // "ABCFGHIJ  " - shifted left, blanks at end
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');
    assert_eq!(row.get(2).unwrap().char(), 'C');
    assert_eq!(row.get(3).unwrap().char(), 'F');
    assert_eq!(row.get(4).unwrap().char(), 'G');
    assert_eq!(row.get(5).unwrap().char(), 'H');
    assert_eq!(row.get(6).unwrap().char(), 'I');
    assert_eq!(row.get(7).unwrap().char(), 'J');
    assert_eq!(row.get(8).unwrap().char(), ' ');
    assert_eq!(row.get(9).unwrap().char(), ' ');
}

#[test]
fn row_delete_chars_tail_overlap() {
    let (_pages, mut row) = make_row(10);
    row.write_char(0, 'A');
    row.write_char(5, 'B');
    assert_eq!(row.len(), 6);

    row.delete_chars(4, 3);
    assert_eq!(row.len(), 1);
    assert_eq!(row.get(0).unwrap().char(), 'A');
}

#[test]
fn row_insert_chars_at_end() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABC".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    // Insert at position past content
    row.insert_chars(8, 2);
    // Should work, inserting blanks at position 8
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');
    assert_eq!(row.get(2).unwrap().char(), 'C');
}

#[test]
fn row_insert_chars_truncation_drops_tail() {
    let (_pages, mut row) = make_row(6);
    row.write_char(5, 'Z');
    assert_eq!(row.len(), 6);

    row.insert_chars(0, 2);
    assert_eq!(row.len(), 0);
    assert!(row.is_empty());
}

#[test]
fn row_delete_chars_more_than_remaining() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    // Delete 20 chars at position 5 (more than available)
    row.delete_chars(5, 20);
    // Should delete F-J, leaving "ABCDE     "
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(4).unwrap().char(), 'E');
    assert_eq!(row.get(5).unwrap().char(), ' ');
}

#[test]
fn row_erase_chars() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.to_string(), "ABCDEFGHIJ");

    // Erase 3 chars at position 3 (D, E, F)
    row.erase_chars(3, 3);
    // "ABC   GHIJ" - no shifting, just blanks in place
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');
    assert_eq!(row.get(2).unwrap().char(), 'C');
    assert_eq!(row.get(3).unwrap().char(), ' ');
    assert_eq!(row.get(4).unwrap().char(), ' ');
    assert_eq!(row.get(5).unwrap().char(), ' ');
    assert_eq!(row.get(6).unwrap().char(), 'G');
    assert_eq!(row.get(7).unwrap().char(), 'H');
    assert_eq!(row.get(8).unwrap().char(), 'I');
    assert_eq!(row.get(9).unwrap().char(), 'J');
}

#[test]
fn row_erase_chars_beyond_end() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDE".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    // Erase 100 chars at position 3 (should stop at row end)
    row.erase_chars(3, 100);
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');
    assert_eq!(row.get(2).unwrap().char(), 'C');
    assert_eq!(row.get(3).unwrap().char(), ' ');
    assert_eq!(row.get(4).unwrap().char(), ' ');
}

#[test]
fn row_erase_chars_zero_count() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDE".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    // Erase 0 chars - should do nothing
    row.erase_chars(2, 0);
    assert_eq!(row.to_string(), "ABCDE");
}

#[test]
fn row_write_overwrite_wide_continuation() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(80);

    // Write a wide char at col 0
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());

    // Verify wide char setup
    let cell0 = row.get(0).unwrap();
    let cell1 = row.get(1).unwrap();
    assert!(cell0.is_wide(), "Cell 0 should be wide");
    assert!(
        cell1.is_wide_continuation(),
        "Cell 1 should be continuation"
    );

    // Overwrite col 1 (continuation) with 'A'
    row.write_char_styled(1, 'A', fg, bg, CellFlags::empty());

    // Check result
    let cell0_after = row.get(0).unwrap();
    let cell1_after = row.get(1).unwrap();
    assert_eq!(cell1_after.char(), 'A', "Cell 1 should be 'A'");
    assert_eq!(cell0_after.char(), ' ', "Cell 0 should be cleared to space");
}

// ── fill_cell_run ────────────────────────────────────────────────

#[test]
fn fill_cell_run_basic() {
    let (_pages, mut row) = make_row(80);
    let template = Cell::from_ascii_fast(b'X');
    let written = row.fill_cell_run(0, 5, template);
    assert_eq!(written, 5);
    assert_eq!(row.len(), 5);
    for col in 0..5 {
        assert_eq!(row.get(col).unwrap().char(), 'X');
    }
    // Next cell is still empty.
    assert_eq!(row.get(5).unwrap().char(), ' ');
}

#[test]
fn fill_cell_run_with_style() {
    use crate::{CellFlags, PackedColors};

    let (_pages, mut row) = make_row(80);
    let colors = PackedColors::DEFAULT.set_fg_indexed(1);
    let template = Cell::from_ascii_styled(b'-', colors, CellFlags::BOLD);
    let written = row.fill_cell_run(10, 20, template);
    assert_eq!(written, 20);
    assert_eq!(row.len(), 30);
    for col in 10..30 {
        let cell = row.get(col).unwrap();
        assert_eq!(cell.char(), '-');
        assert!(cell.flags().contains(CellFlags::BOLD));
    }
}

#[test]
fn fill_cell_run_clamps_to_row_end() {
    let (_pages, mut row) = make_row(10);
    let template = Cell::from_ascii_fast(b'Z');
    // Request 20 cells but only 7 fit (cols 3..10).
    let written = row.fill_cell_run(3, 20, template);
    assert_eq!(written, 7);
    assert_eq!(row.len(), 10);
    for col in 3..10 {
        assert_eq!(row.get(col).unwrap().char(), 'Z');
    }
}

#[test]
fn fill_cell_run_zero_count() {
    let (_pages, mut row) = make_row(10);
    let template = Cell::from_ascii_fast(b'A');
    let written = row.fill_cell_run(0, 0, template);
    assert_eq!(written, 0);
    assert!(row.is_empty());
}

#[test]
fn fill_cell_run_out_of_bounds_col() {
    let (_pages, mut row) = make_row(10);
    let template = Cell::from_ascii_fast(b'A');
    let written = row.fill_cell_run(100, 5, template);
    assert_eq!(written, 0);
    assert!(row.is_empty());
}

#[test]
fn fill_cell_run_overwrites_wide_chars() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(80);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write a wide char at col 4-5.
    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());

    // Fill over col 5 (continuation) with 'A'.
    // Should clear the orphaned first half at col 4.
    let template = Cell::from_ascii_fast(b'A');
    row.fill_cell_run(5, 3, template);
    assert_eq!(
        row.get(4).unwrap().char(),
        ' ',
        "orphaned wide main cleared"
    );
    assert_eq!(row.get(5).unwrap().char(), 'A');
    assert_eq!(row.get(6).unwrap().char(), 'A');
    assert_eq!(row.get(7).unwrap().char(), 'A');
}

#[test]
fn fill_cell_run_full_row() {
    let (_pages, mut row) = make_row(10);
    let template = Cell::from_ascii_fast(b'#');
    let written = row.fill_cell_run(0, 10, template);
    assert_eq!(written, 10);
    assert_eq!(row.len(), 10);
    for col in 0..10 {
        assert_eq!(row.get(col).unwrap().char(), '#');
    }
}
