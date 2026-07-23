// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
// write_ascii_blast content verification
// ========================================================================

#[test]
fn write_ascii_blast_content_matches_write_char() {
    let mut grid_blast = Grid::new(5, 10);
    let mut grid_char = Grid::new(5, 10);

    let text = b"Hello!";
    grid_blast.write_ascii_blast(text);
    for &byte in text {
        grid_char.write_char(byte as char);
    }

    for col in 0..text.len() as u16 {
        let blast_char = grid_blast.cell(0, col).unwrap().char();
        let char_char = grid_char.cell(0, col).unwrap().char();
        assert_eq!(blast_char, char_char, "cell content mismatch at col {col}",);
    }
}

#[test]
fn write_ascii_blast_fills_exact_line() {
    let mut grid = Grid::new(3, 5);
    grid.write_ascii_blast(b"ABCDE");

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(0, 3).unwrap().char(), 'D');
    assert_eq!(grid.cell(0, 4).unwrap().char(), 'E');
    // Deferred wrap: cursor at last col, not yet wrapped
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 4);
    assert!(grid.pending_wrap());
}

#[test]
fn write_ascii_blast_wraps_and_verifies_content_across_lines() {
    let mut grid = Grid::new(3, 4);
    grid.write_ascii_blast(b"ABCDEFGH");

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 3).unwrap().char(), 'D');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'E');
    assert_eq!(grid.cell(1, 3).unwrap().char(), 'H');
    assert!(grid.row(1).unwrap().is_wrapped());
}

#[test]
fn write_ascii_blast_returns_correct_written_count() {
    let mut grid = Grid::new(2, 5);
    let written = grid.write_ascii_blast(b"ABCDEFGHIJKLMNO");
    assert_eq!(written, 15);
}

#[test]
fn write_ascii_blast_empty_input() {
    let mut grid = Grid::new(3, 10);
    let written = grid.write_ascii_blast(b"");
    assert_eq!(written, 0);
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn write_ascii_blast_single_byte() {
    let mut grid = Grid::new(3, 10);
    let written = grid.write_ascii_blast(b"X");
    assert_eq!(written, 1);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'X');
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn write_ascii_blast_at_cursor_offset() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 5);
    grid.write_ascii_blast(b"Hello");

    assert_eq!(grid.cell(0, 5).unwrap().char(), 'H');
    assert_eq!(grid.cell(0, 9).unwrap().char(), 'o');
    assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 4).unwrap().char(), ' ');
    // Deferred wrap: cursor stays at last col with pending_wrap flag
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 9);
    assert!(grid.pending_wrap());
    // Resolving the wrap moves cursor to next line
    grid.resolve_pending_wrap();
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn write_ascii_blast_scrolls_at_screen_bottom() {
    let mut grid = Grid::new(2, 3);

    let written = grid.write_ascii_blast(b"ABCDEF");
    assert_eq!(written, 6);

    // Deferred wrap: cursor at last col of row 1, pending_wrap set.
    // Scroll hasn't happened yet.
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 2);
    assert!(grid.pending_wrap());

    // Resolve deferred wrap → triggers scroll_region_up(1)
    grid.resolve_pending_wrap();
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'E');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'F');
    assert!(grid.row(1).unwrap().is_empty());
}

// ========================================================================
// Carriage return
// ========================================================================

#[test]
fn carriage_return_resets_column_to_zero() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(3, 7);
    grid.carriage_return();

    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(grid.cursor_row(), 3);
}

#[test]
fn carriage_return_at_col_zero_is_noop() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(2, 0);
    grid.carriage_return();

    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(grid.cursor_row(), 2);
}

#[test]
fn carriage_return_then_line_feed_sequence() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(2, 8);

    grid.carriage_return();
    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(grid.cursor_row(), 2);

    grid.line_feed();
    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(grid.cursor_row(), 3);
}

// ========================================================================
// Erase rect boundary conditions
// ========================================================================

#[test]
fn erase_rect_basic() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(1, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.erase_rect(1, 2, 1, 5);

    assert_eq!(grid.cell(1, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 1).unwrap().char(), 'B');
    assert_eq!(grid.cell(1, 2).unwrap().char(), ' ');
    assert_eq!(grid.cell(1, 5).unwrap().char(), ' ');
    assert_eq!(grid.cell(1, 6).unwrap().char(), 'G');
}

#[test]
fn erase_rect_inverted_coordinates_is_noop() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(0, 0);
    grid.write_char('X');

    grid.erase_rect(3, 0, 1, 5);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'X');

    grid.erase_rect(0, 5, 2, 2);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'X');
}

#[test]
fn erase_rect_exceeding_grid_bounds_clamped() {
    let mut grid = Grid::new(3, 5);
    fill_grid_rows(&mut grid, 3);

    grid.erase_rect(0, 0, 100, 100);

    for row in 0..3 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be cleared",
        );
    }
}

#[test]
fn erase_rect_single_cell() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(1, 5);
    grid.write_char('X');

    grid.erase_rect(1, 5, 1, 5);
    assert_eq!(grid.cell(1, 5).unwrap().char(), ' ');
}

// ========================================================================
// Backspace boundary
// ========================================================================

#[test]
fn backspace_at_col_zero_stays() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(2, 0);
    grid.backspace();

    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(grid.cursor_row(), 2);
}

#[test]
fn backspace_from_mid_column() {
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(1, 5);
    grid.backspace();

    assert_eq!(grid.cursor_col(), 4);
    assert_eq!(grid.cursor_row(), 1);
}

// ========================================================================
// Selective erase boundary conditions
// ========================================================================

#[test]
fn selective_erase_to_end_of_screen_preserves_protected() {
    let mut grid = Grid::new(3, 5);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }

    if let Some(cell) = grid.cell_mut(0, 2) {
        let mut flags = cell.flags();
        flags.insert(CellFlags::PROTECTED);
        cell.set_flags(flags);
    }

    grid.set_cursor(0, 0);
    grid.selective_erase_to_end_of_screen();

    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 1).unwrap().char(), ' ');
}

// Algorithm audit: Row insert_chars/delete_chars boundary conditions
// ========================================================================

/// insert_chars with count larger than available space clips correctly.
#[test]
fn insert_chars_count_exceeds_available_space() {
    let mut grid = Grid::new(3, 5);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }

    // Insert 100 chars at col 2 — should clip to what fits
    grid.set_cursor(0, 2);
    if let Some(row) = grid.row_mut(0) {
        row.insert_chars(2, 100);
    }

    // Cols 0-1 should be preserved
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    // Cols 2-4 should be empty (inserted blanks, original CDE shifted out)
    for col in 2..5 {
        assert!(
            grid.cell(0, col).unwrap().is_empty(),
            "col {col} should be empty after large insert",
        );
    }
}

/// delete_chars with count larger than remaining should clear from cursor to end.
#[test]
fn delete_chars_count_exceeds_remaining() {
    let mut grid = Grid::new(3, 5);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }

    if let Some(row) = grid.row_mut(0) {
        row.delete_chars(3, 100);
    }

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    // Cols 3-4 should be empty (nothing left to shift in)
    assert!(grid.cell(0, 3).unwrap().is_empty());
    assert!(grid.cell(0, 4).unwrap().is_empty());
}

/// insert_chars at the last column should only affect that column.
#[test]
fn insert_chars_at_last_column() {
    let mut grid = Grid::new(3, 5);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }

    if let Some(row) = grid.row_mut(0) {
        row.insert_chars(4, 1);
    }

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(0, 3).unwrap().char(), 'D');
    // Col 4: original 'E' shifted out, blank inserted
    assert!(
        grid.cell(0, 4).unwrap().is_empty(),
        "last col should be blank after insert"
    );
}

/// erase_chars at the end of content: should clear without affecting other cells.
#[test]
fn erase_chars_at_content_boundary() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABC".chars() {
        grid.write_char(c);
    }

    // Erase 2 chars starting at col 2 (the last content cell)
    if let Some(row) = grid.row_mut(0) {
        row.erase_chars(2, 2);
    }

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    assert!(
        grid.cell(0, 2).unwrap().is_empty(),
        "erased cell should be empty"
    );
    assert!(
        grid.cell(0, 3).unwrap().is_empty(),
        "cell past content should stay empty"
    );
}

// ========================================================================
// ECH/ICH/DCH extras management (#4057)
// ========================================================================

use crate::extra::CellCoord;

/// ECH: Erasing characters clears extras at erased positions.
#[test]
fn erase_chars_clears_extras() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    // Add extras at cols 1, 2, 3
    for col in 1..4u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .add_combining('\u{0301}');
    }
    // Verify all 3 extras exist before operation
    assert!(grid.extras().get(CellCoord::new(0, 1)).is_some());
    assert!(grid.extras().get(CellCoord::new(0, 2)).is_some());
    assert!(grid.extras().get(CellCoord::new(0, 3)).is_some());

    // ECH 2 at col 2 → erases cols 2, 3
    grid.set_cursor(0, 2);
    grid.erase_chars(2);

    // Col 1 extras should survive, cols 2-3 should be cleared
    assert!(
        grid.extras().get(CellCoord::new(0, 1)).is_some(),
        "col 1 extras preserved"
    );
    assert!(
        grid.extras().get(CellCoord::new(0, 2)).is_none(),
        "col 2 extras cleared by ECH"
    );
    assert!(
        grid.extras().get(CellCoord::new(0, 3)).is_none(),
        "col 3 extras cleared by ECH"
    );
}

/// ICH: Inserting characters shifts extras right; blanks have no extras.
#[test]
fn insert_chars_shifts_extras_right() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    // Add extras at cols 1 and 4
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 1))
        .add_combining('\u{0301}');
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 4))
        .add_combining('\u{0302}');

    // ICH 2 at col 2 → shifts cols 2+ right by 2
    grid.set_cursor(0, 2);
    grid.insert_chars(2);

    // Col 1 < insert point: preserved at col 1
    assert!(
        grid.extras().get(CellCoord::new(0, 1)).is_some(),
        "col 1 extras preserved (before insert point)"
    );
    // Col 4 shifted to col 6
    assert!(
        grid.extras().get(CellCoord::new(0, 4)).is_none(),
        "col 4 extras shifted away"
    );
    assert!(
        grid.extras().get(CellCoord::new(0, 6)).is_some(),
        "col 4 extras shifted to col 6"
    );
    // No extras at the newly inserted blank positions (cols 2, 3)
    assert!(
        grid.extras().get(CellCoord::new(0, 2)).is_none(),
        "inserted blank col 2 has no extras"
    );
    assert!(
        grid.extras().get(CellCoord::new(0, 3)).is_none(),
        "inserted blank col 3 has no extras"
    );
}

/// DCH: Deleting characters removes extras and shifts remaining left.
#[test]
fn delete_chars_shifts_extras_left() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGH".chars() {
        grid.write_char(c);
    }
    // Add extras at cols 1, 3, 6
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 1))
        .add_combining('\u{0301}');
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 3))
        .add_combining('\u{0302}');
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 6))
        .add_combining('\u{0303}');

    // DCH 2 at col 2 → deletes cols 2-3, shifts cols 4+ left by 2
    grid.set_cursor(0, 2);
    grid.delete_chars(2);

    // Col 1 < delete point: preserved
    assert!(
        grid.extras().get(CellCoord::new(0, 1)).is_some(),
        "col 1 extras preserved (before delete point)"
    );
    // Col 3 was in deletion range [2, 4): deleted
    assert!(
        grid.extras().get(CellCoord::new(0, 3)).is_none(),
        "col 3 extras deleted by DCH"
    );
    // Col 6 shifted left by 2 → col 4
    assert!(
        grid.extras().get(CellCoord::new(0, 6)).is_none(),
        "col 6 extras shifted away"
    );
    assert!(
        grid.extras().get(CellCoord::new(0, 4)).is_some(),
        "col 6 extras shifted to col 4"
    );
}
