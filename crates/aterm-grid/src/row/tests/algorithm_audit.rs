// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// Algorithm audit tests: insert/delete chars wide character invariants,
// len tracking edge cases, selective_clear_range len recalculation,
// and recalculate_len_up_to boundary conditions.

use super::super::*;
use super::make_row;

// insert_chars / delete_chars wide character invariants

/// ICH (insert chars) at a position that bisects a wide character pair.
/// Regression test for #2412.
#[test]
fn insert_chars_at_wide_continuation_clears_orphan() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write wide char at cols 2-3
    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    // Insert 1 blank at col 3 (the continuation cell).
    row.insert_chars(3, 1);

    let cell2 = row.get(2).unwrap();
    let cell3 = row.get(3).unwrap();

    // After insert at the continuation position, col 2's WIDE cell has no
    // adjacent continuation at col 3 (it shifted to col 4). The orphaned
    // first half is cleared by wide char fixup.
    assert_eq!(
        cell2.flags() & CellFlags::WIDE,
        CellFlags::empty(),
        "orphaned WIDE cell at col 2 should be cleared"
    );
    assert_eq!(
        cell3.flags() & CellFlags::WIDE_CONTINUATION,
        CellFlags::empty(),
        "col 3 should be blank (inserted), not continuation"
    );
}

/// DCH (delete chars) removing the WIDE cell should clear the orphaned
/// continuation. Regression test for #2412.
#[test]
fn delete_chars_splits_wide_char_at_boundary() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write "A" at col 0, wide char at cols 1-2, "B" at col 3
    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_wide_char(1, '\u{4E2D}', fg, bg, CellFlags::empty());
    row.write_char_styled(3, 'B', fg, bg, CellFlags::empty());

    // Delete 1 char at col 1 (the WIDE cell).
    row.delete_chars(1, 1);

    let cell0 = row.get(0).unwrap();
    let cell1 = row.get(1).unwrap();

    assert_eq!(cell0.char(), 'A', "col 0 should be unchanged");

    // After delete, the WIDE_CONTINUATION from col 2 shifts to col 1.
    // It is cleared by wide char fixup since it no longer has a preceding WIDE cell.
    assert_eq!(
        cell1.flags() & CellFlags::WIDE_CONTINUATION,
        CellFlags::empty(),
        "orphaned WIDE_CONTINUATION at col 1 should be cleared"
    );
    // 'B' should have shifted from col 3 to col 2
    assert_eq!(
        row.get(2).unwrap().char(),
        'B',
        "'B' should shift from col 3 to col 2"
    );
}

/// delete_chars removing only the continuation of a wide char
/// should clear the orphaned WIDE cell. Regression test for #2412.
#[test]
fn delete_chars_removes_continuation_orphans_wide() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Wide char at cols 4-5, then "XY" at 6-7
    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    row.write_char_styled(6, 'X', fg, bg, CellFlags::empty());
    row.write_char_styled(7, 'Y', fg, bg, CellFlags::empty());

    // Delete 1 char at col 5 (the continuation).
    row.delete_chars(5, 1);

    let cell4 = row.get(4).unwrap();
    let cell5 = row.get(5).unwrap();

    // Col 4's WIDE flag is cleared by wide char fixup since its continuation
    // at col 5 was deleted.
    assert_eq!(
        cell4.flags() & CellFlags::WIDE,
        CellFlags::empty(),
        "orphaned WIDE cell at col 4 should be cleared"
    );
    assert_eq!(cell5.char(), 'X', "'X' should shift from col 6 to col 5");
    assert_eq!(
        row.get(6).unwrap().char(),
        'Y',
        "'Y' should shift from col 7 to col 6"
    );
}

// insert_chars len tracking edge cases

/// insert_chars with count > remaining space should clamp correctly.
#[test]
fn insert_chars_count_exceeds_remaining_space() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDE".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.len(), 5);

    // Insert 20 blanks at col 2 — much more than remaining space
    row.insert_chars(2, 20);

    // Cols 0-1 should be unchanged (before insertion point)
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(1).unwrap().char(), 'B');

    // Cols 2-9 should be blank (insertion fills them)
    for col in 2..10u16 {
        assert_eq!(
            row.get(col).unwrap().char(),
            ' ',
            "col {col} should be blank after oversized insert"
        );
    }
}

/// insert_chars at col 0 should shift all content right.
#[test]
fn insert_chars_at_col_zero() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABC".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    row.insert_chars(0, 3);
    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert_eq!(row.get(1).unwrap().char(), ' ');
    assert_eq!(row.get(2).unwrap().char(), ' ');
    assert_eq!(row.get(3).unwrap().char(), 'A');
    assert_eq!(row.get(4).unwrap().char(), 'B');
    assert_eq!(row.get(5).unwrap().char(), 'C');
    assert_eq!(row.len(), 6);
}

/// delete_chars at col 0 should shift all content left.
#[test]
fn delete_chars_at_col_zero() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDE".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    row.delete_chars(0, 2);
    assert_eq!(row.get(0).unwrap().char(), 'C');
    assert_eq!(row.get(1).unwrap().char(), 'D');
    assert_eq!(row.get(2).unwrap().char(), 'E');
    assert_eq!(row.get(3).unwrap().char(), ' ');
    assert_eq!(row.len(), 3);
}

// selective_clear_range len recalculation

/// selective_clear_range should correctly update len when the last non-empty
/// cell is protected and the cell at old_len-1 was erased.
#[test]
fn selective_clear_range_len_with_protected_at_end() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write "ABCPD" where P is protected at col 3
    row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
    row.write_char_styled(1, 'B', fg, bg, CellFlags::empty());
    row.write_char_styled(2, 'C', fg, bg, CellFlags::empty());
    row.write_char_styled(3, 'P', fg, bg, CellFlags::PROTECTED);
    row.write_char_styled(4, 'D', fg, bg, CellFlags::empty());
    assert_eq!(row.len(), 5);

    // Selectively clear the entire range 0..5
    // Should erase A, B, C, D but NOT P
    row.selective_clear_range(0, 5);

    assert_eq!(
        row.get(3).unwrap().char(),
        'P',
        "protected cell should survive"
    );
    assert_eq!(row.get(4).unwrap().char(), ' ', "D should be erased");

    // len should be 4 (protected cell at col 3 is the last non-empty)
    assert_eq!(
        row.len(),
        4,
        "len should account for protected cell at col 3"
    );
}

/// selective_clear on a row where ALL cells are protected should not change len.
#[test]
fn selective_clear_all_protected_preserves_len() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(5);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    for i in 0..5u16 {
        row.write_char_styled(i, char::from(b'A' + i as u8), fg, bg, CellFlags::PROTECTED);
    }
    assert_eq!(row.len(), 5);

    row.selective_clear();

    // Nothing should change since all cells are protected
    assert_eq!(row.len(), 5);
    for i in 0..5u16 {
        assert_eq!(
            row.get(i).unwrap().char(),
            char::from(b'A' + i as u8),
            "protected cell at col {i} should survive"
        );
    }
}

// recalculate_len_up_to boundary conditions

/// clear_range in the middle of content should NOT change len.
#[test]
fn clear_range_middle_preserves_len() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.len(), 10);

    // Clear cols 3-5 (middle of content)
    row.clear_range(3, 6);
    // Content should be: "ABC   GHIJ"
    // len should still be 10 since cols 6-9 have content
    assert_eq!(
        row.len(),
        10,
        "len should remain 10 when clearing middle of row"
    );
    assert_eq!(row.get(6).unwrap().char(), 'G');
    assert_eq!(row.get(9).unwrap().char(), 'J');
}

/// erase_chars in the middle of content should NOT change len.
#[test]
fn erase_chars_middle_preserves_len() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDEFGHIJ".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }
    assert_eq!(row.len(), 10);

    // Erase 3 chars at col 2
    row.erase_chars(2, 3);
    // "AB   FGHIJ"
    assert_eq!(
        row.len(),
        10,
        "len should remain 10 when erasing middle of row"
    );
    assert_eq!(row.get(5).unwrap().char(), 'F');
}

/// ECH of a WIDE head whose continuation is the row's LAST content cell must
/// leave `Row::len` correct. Regression: the len-recalc guard used
/// `end >= old_len`, which is false when the wide pair is the row's tail
/// (old_len == end + 1), so the separately-cleared orphan continuation at
/// index `end` (== old_len - 1) left len overstated and leaked trailing blanks.
#[test]
fn erase_chars_wide_head_at_tail_recalcs_len() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Content: A, B, then a wide char at cols 2-3. cells = A,B,WIDE,CONT; len=4.
    row.write_char(0, 'A');
    row.write_char(1, 'B');
    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert_eq!(row.len(), 4);
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    // ECH at col 2, count 1: end=3. cells[2] (WIDE) erased and the orphan
    // continuation at cells[3] cleared, leaving content only at cols 0-1.
    row.erase_chars(2, 1);

    assert_eq!(
        row.len(),
        2,
        "len must shrink to 2 after erasing the tail wide pair"
    );
    assert_eq!(row.to_string(), "AB", "no trailing blanks should leak");
    assert!(row.get(2).unwrap().is_empty());
    assert!(row.get(3).unwrap().is_empty());
}

/// clear_from at col 0 should set len to 0.
#[test]
fn clear_from_zero_sets_len_to_zero() {
    let (_pages, mut row) = make_row(10);
    for i in 0..10u16 {
        row.write_char(i, 'X');
    }
    assert_eq!(row.len(), 10);

    row.clear_from(0);
    assert_eq!(row.len(), 0, "clear_from(0) should leave len at 0");
}

/// delete_chars at the last column with a large count should handle gracefully.
#[test]
fn delete_chars_at_last_col() {
    let (_pages, mut row) = make_row(10);
    for (i, c) in "ABCDE".chars().enumerate() {
        row.write_char(u16_from_usize(i), c);
    }

    // Delete 5 at col 9 (last column, which is empty)
    row.delete_chars(9, 5);
    assert_eq!(row.len(), 5, "deleting at empty tail should not change len");
    assert_eq!(row.get(0).unwrap().char(), 'A');
}

// Row::set col+1 boundary tests
//
// Row::set computes `self.len = col + 1` when updating the length tracker.
// This could overflow u16 if col == u16::MAX (65535). The overflow is
// currently unreachable because:
//   - cells is allocated from cols: u16, so cells.len() <= 65535
//   - get_mut(65535) on a 65535-element vec fails (valid indices: 0..65534)
//   - Therefore the branch containing `col + 1` is never reached for col=65535
//
// These tests document the invariant: writing to the last valid column
// (cols-1) must succeed and produce len=cols without overflow.

/// Row::set at the last valid column should update len to cols.
#[test]
fn row_set_last_valid_column_updates_len() {
    let (_pages, mut row) = make_row(100);
    let cell = Cell::new('X');

    assert!(row.set(99, cell), "set at last column (99) should succeed");
    assert_eq!(
        row.len(),
        100,
        "len should be cols (100) after writing last column"
    );

    // Out-of-bounds write should fail and leave len unchanged
    assert!(!row.set(100, cell), "set at col 100 should fail (OOB)");
    assert_eq!(row.len(), 100, "len unchanged after OOB set");
}

/// Row::set at col=0 on a 1-column row: tests the smallest row size.
#[test]
fn row_set_single_column_row() {
    let (_pages, mut row) = make_row(1);
    let cell = Cell::new('X');

    assert_eq!(row.cols(), 1);
    assert_eq!(row.len(), 0);

    assert!(row.set(0, cell), "set at col 0 should succeed");
    assert_eq!(row.len(), 1, "len should be 1 after writing sole column");

    // Col 1 is out of bounds
    assert!(!row.set(1, cell), "set at col 1 should fail (OOB)");
    assert_eq!(row.len(), 1);
}

/// Row::set at last column of a large row (8192 columns — PAGE_SIZE limit).
/// Verifies that col + 1 arithmetic is correct at the practical maximum:
/// cols = 8192, last valid col = 8191, len = 8191 + 1 = 8192.
///
/// Note: u16::MAX (65535) exceeds PAGE_SIZE and cannot be allocated.
/// The col + 1 overflow at u16::MAX is unreachable because the page
/// allocator rejects rows wider than PAGE_SIZE / sizeof(Cell) ≈ 8192.
#[test]
fn row_set_page_max_cols_last_valid() {
    // 8192 cells × 8 bytes = 65536 = PAGE_SIZE exactly
    let (_pages, mut row) = make_row(8192);
    let cell = Cell::new('X');

    let last_col: u16 = 8191;
    assert!(
        row.set(last_col, cell),
        "set at col 8191 should succeed on 8192-col row"
    );
    assert_eq!(row.len(), 8192, "len should be 8192 after writing col 8191");

    // Col 8192 is out of bounds
    assert!(
        !row.set(8192, cell),
        "set at col 8192 should fail (OOB on 8192-element row)"
    );
}

/// Row::set does NOT overflow on repeated writes to the same column.
#[test]
fn row_set_repeated_same_column_no_len_regression() {
    let (_pages, mut row) = make_row(10);
    let cell = Cell::new('X');

    // Write to col 5 three times — len should stay at 6
    assert!(row.set(5, cell));
    assert_eq!(row.len(), 6);

    assert!(row.set(5, cell));
    assert_eq!(row.len(), 6, "repeated set should not change len");

    assert!(row.set(5, cell));
    assert_eq!(row.len(), 6, "len stable after third set");
}
