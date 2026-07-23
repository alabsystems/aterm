// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boundary condition tests — min dimensions, zero counts, edge cases, write
//! consistency, and history line API ordering. Sourced from algorithm audit #1920.

use super::super::*;

fn write_marker_line(grid: &mut Grid, marker: char) {
    grid.carriage_return();
    grid.write_char(marker);
    grid.carriage_return();
    grid.line_feed();
}

// =========================================================================
// Minimum dimension / clamping
// =========================================================================

#[test]
fn grid_new_minimum_1x1() {
    let grid = Grid::new(1, 1);
    assert_eq!(grid.rows(), 1, "1x1 grid: 1 row");
    assert_eq!(grid.cols(), 1, "1x1 grid: 1 column");
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
    grid.assert_invariants();
}

#[test]
fn grid_new_zero_dims_clamped_to_1() {
    // Grid::with_scrollback clamps rows.max(1), cols.max(1)
    let grid = Grid::new(0, 0);
    assert_eq!(grid.rows(), 1, "0x0 should be clamped to 1x1");
    assert_eq!(grid.cols(), 1, "0x0 should be clamped to 1x1");
    grid.assert_invariants();
}

#[test]
fn grid_new_zero_rows_clamped() {
    let grid = Grid::new(0, 80);
    assert_eq!(grid.rows(), 1, "0 rows clamped to 1");
    assert_eq!(grid.cols(), 80);
    grid.assert_invariants();
}

#[test]
fn grid_new_zero_cols_clamped() {
    let grid = Grid::new(24, 0);
    assert_eq!(grid.rows(), 24);
    assert_eq!(grid.cols(), 1, "0 cols clamped to 1");
    grid.assert_invariants();
}

#[test]
fn grid_write_char_1x1_stays_at_origin() {
    let mut grid = Grid::new(1, 1);

    // Write to the only cell — cursor should stay at (0, 0)
    grid.write_char('X');
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(
        grid.cursor_col(),
        0,
        "1x1: cursor stays at col 0 after write"
    );
    grid.assert_invariants();
}

// =========================================================================
// Zero-count / no-op operations
// =========================================================================

#[test]
fn grid_resize_to_same_size_is_noop() {
    let mut grid = Grid::new(24, 80);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    let orig_row = grid.cursor_row();
    let orig_col = grid.cursor_col();

    grid.resize(24, 80);

    assert_eq!(grid.rows(), 24);
    assert_eq!(grid.cols(), 80);
    assert_eq!(grid.cursor_row(), orig_row);
    assert_eq!(grid.cursor_col(), orig_col);
    grid.assert_invariants();
}

#[test]
fn grid_insert_chars_zero_count() {
    let mut grid = Grid::new(24, 80);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 2);
    grid.insert_chars(0);

    // Should be a no-op
    assert_eq!(grid.cursor_col(), 2);
    grid.assert_invariants();
}

#[test]
fn grid_delete_chars_zero_count() {
    let mut grid = Grid::new(24, 80);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 2);
    grid.delete_chars(0);

    assert_eq!(grid.cursor_col(), 2);
    grid.assert_invariants();
}

#[test]
fn grid_scroll_up_zero() {
    let mut grid = Grid::new(24, 80);
    for c in "Test".chars() {
        grid.write_char(c);
    }
    let orig_row = grid.cursor_row();
    let orig_col = grid.cursor_col();

    grid.scroll_up(0);

    assert_eq!(grid.cursor_row(), orig_row);
    assert_eq!(grid.cursor_col(), orig_col);
    grid.assert_invariants();
}

#[test]
fn grid_scroll_down_zero() {
    let mut grid = Grid::new(24, 80);
    grid.scroll_down(0);
    grid.assert_invariants();
}

// =========================================================================
// Exceeding-count operations
// =========================================================================

#[test]
fn grid_insert_lines_exceeding_available() {
    let mut grid = Grid::new(5, 10);
    // Write markers on rows 0-1 to verify they are preserved
    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.set_cursor(2, 0);
    grid.write_char('C');

    // Insert 100 lines from row 2: only 3 rows below, so clamped to region
    grid.insert_lines(100);
    grid.assert_invariants();

    // Rows above cursor (0, 1) must be preserved
    assert_eq!(
        grid.cell(0, 0).unwrap().char(),
        'A',
        "row 0 should be preserved"
    );
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'B',
        "row 1 should be preserved"
    );
    // Rows 2-4 should be blank (cleared by insert)
    assert_eq!(
        grid.cell(2, 0).unwrap().char(),
        ' ',
        "row 2 should be blank after insert"
    );
    assert_eq!(
        grid.cell(3, 0).unwrap().char(),
        ' ',
        "row 3 should be blank after insert"
    );
    assert_eq!(
        grid.cell(4, 0).unwrap().char(),
        ' ',
        "row 4 should be blank after insert"
    );
}

#[test]
fn grid_delete_lines_exceeding_available() {
    let mut grid = Grid::new(5, 10);
    // Write markers to verify behavior
    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.set_cursor(2, 0);
    grid.write_char('C');
    grid.set_cursor(3, 0);
    grid.write_char('D');

    grid.set_cursor(2, 0);
    // Delete 100 lines from row 2: only 3 rows available, all deleted
    grid.delete_lines(100);
    grid.assert_invariants();

    // Rows above cursor (0, 1) must be preserved
    assert_eq!(
        grid.cell(0, 0).unwrap().char(),
        'A',
        "row 0 should be preserved"
    );
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'B',
        "row 1 should be preserved"
    );
    // Rows 2-4 should be blank (content deleted, blanks inserted at bottom)
    assert_eq!(
        grid.cell(2, 0).unwrap().char(),
        ' ',
        "row 2 should be blank after delete"
    );
    assert_eq!(
        grid.cell(3, 0).unwrap().char(),
        ' ',
        "row 3 should be blank after delete"
    );
    assert_eq!(
        grid.cell(4, 0).unwrap().char(),
        ' ',
        "row 4 should be blank after delete"
    );
}

// =========================================================================
// Edge-position operations
// =========================================================================

#[test]
fn grid_restore_cursor_without_save() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(10, 40);

    // Restore without prior save — should not panic, implementation-defined behavior
    grid.restore_cursor();
    grid.assert_invariants();
}

#[test]
fn grid_insert_chars_at_last_column() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 79);
    grid.insert_chars(5);
    // Should not panic; insert at last column has no visible effect
    grid.assert_invariants();
}

#[test]
fn grid_delete_chars_at_last_column() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 79);
    grid.delete_chars(5);
    grid.assert_invariants();
}

#[test]
fn grid_insert_chars_at_col_zero() {
    let mut grid = Grid::new(24, 80);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 0);
    grid.insert_chars(3);

    // Content should shift right by 3 columns
    assert_eq!(grid.cursor_col(), 0);
    grid.assert_invariants();
}

#[test]
fn grid_erase_scrollback_empty() {
    let mut grid = Grid::new(24, 80);
    // No scrollback content — should not panic
    grid.erase_scrollback();
    grid.assert_invariants();
}

#[test]
fn grid_cursor_at_max_position_then_resize_down() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(23, 79);
    assert_eq!(grid.cursor_row(), 23);
    assert_eq!(grid.cursor_col(), 79);

    grid.resize(10, 40);
    // Cursor should be clamped
    assert!(grid.cursor_row() < 10, "cursor row clamped after resize");
    assert!(grid.cursor_col() < 40, "cursor col clamped after resize");
    grid.assert_invariants();
}

// =========================================================================
// Write consistency — ascii_blast vs char_wrap (#2046)
// =========================================================================

/// Verify that write_ascii_blast and write_char_wrap produce identical cursor
/// positions after filling a line to column width.
///
/// Both paths use immediate wrap (no deferred/pending wrap in the grid layer).
/// Regression test for #2046 MEDIUM finding: wrap consistency.
#[test]
fn write_ascii_blast_and_char_wrap_cursor_consistency() {
    // Test 1: Fill exactly one full line via both paths
    let mut grid_blast = Grid::new(5, 10);
    let mut grid_char = Grid::new(5, 10);

    grid_blast.write_ascii_blast(b"0123456789");
    for c in "0123456789".chars() {
        grid_char.write_char_wrap(c);
    }

    assert_eq!(
        grid_blast.cursor_row(),
        grid_char.cursor_row(),
        "cursor row after filling one line"
    );
    assert_eq!(
        grid_blast.cursor_col(),
        grid_char.cursor_col(),
        "cursor col after filling one line"
    );

    // Test 2: Fill partial line
    let mut grid_blast = Grid::new(5, 10);
    let mut grid_char = Grid::new(5, 10);

    grid_blast.write_ascii_blast(b"hello");
    for c in "hello".chars() {
        grid_char.write_char_wrap(c);
    }

    assert_eq!(
        grid_blast.cursor_row(),
        grid_char.cursor_row(),
        "cursor row after partial line"
    );
    assert_eq!(
        grid_blast.cursor_col(),
        grid_char.cursor_col(),
        "cursor col after partial line"
    );

    // Test 3: Fill exactly two full lines (wrapping across line boundary)
    let mut grid_blast = Grid::new(5, 10);
    let mut grid_char = Grid::new(5, 10);

    grid_blast.write_ascii_blast(b"01234567890123456789");
    for c in "01234567890123456789".chars() {
        grid_char.write_char_wrap(c);
    }

    assert_eq!(
        grid_blast.cursor_row(),
        grid_char.cursor_row(),
        "cursor row after filling two lines"
    );
    assert_eq!(
        grid_blast.cursor_col(),
        grid_char.cursor_col(),
        "cursor col after filling two lines"
    );

    // Test 4: Fill line plus one char (wrap + one char on new line)
    let mut grid_blast = Grid::new(5, 10);
    let mut grid_char = Grid::new(5, 10);

    grid_blast.write_ascii_blast(b"0123456789X");
    for c in "0123456789X".chars() {
        grid_char.write_char_wrap(c);
    }

    assert_eq!(
        grid_blast.cursor_row(),
        grid_char.cursor_row(),
        "cursor row after wrap+1"
    );
    assert_eq!(
        grid_blast.cursor_col(),
        grid_char.cursor_col(),
        "cursor col after wrap+1"
    );
}

// =========================================================================
// History line API ordering
// =========================================================================

#[test]
fn grid_history_line_api_covers_ring_buffer_ordering() {
    let mut grid = Grid::with_scrollback(3, 4, 8);

    for marker in ['A', 'B', 'C', 'D', 'E'] {
        write_marker_line(&mut grid, marker);
    }

    assert_eq!(grid.history_line_count(), 3);

    let oldest = grid
        .get_history_line(0)
        .expect("history idx 0 should exist");
    let middle = grid
        .get_history_line(1)
        .expect("history idx 1 should exist");
    let newest = grid
        .get_history_line(2)
        .expect("history idx 2 should exist");
    let newest_rev = grid
        .history_line_rev(0)
        .expect("reverse history idx 0 should exist");
    let oldest_rev = grid
        .history_line_rev(2)
        .expect("reverse history idx 2 should exist");

    assert_eq!(oldest.to_string().chars().next(), Some('A'));
    assert_eq!(middle.to_string().chars().next(), Some('B'));
    assert_eq!(newest.to_string().chars().next(), Some('C'));
    assert!(grid.get_history_line(3).is_none());
    assert_eq!(newest_rev.to_string().chars().next(), Some('C'));
    assert_eq!(oldest_rev.to_string().chars().next(), Some('A'));
    assert!(grid.history_line_rev(3).is_none());
}

// =========================================================================
// row_u16 / clamp_u16 saturation
// =========================================================================

/// `row_u16` saturates to `u16::MAX` for values above the u16 range.
#[test]
fn row_u16_saturates_on_overflow() {
    use super::super::row_u16;

    // In-range values convert exactly.
    assert_eq!(row_u16(0), 0);
    assert_eq!(row_u16(1), 1);
    assert_eq!(row_u16(u16::MAX as usize), u16::MAX);

    // Values above u16::MAX saturate.
    assert_eq!(row_u16(u16::MAX as usize + 1), u16::MAX);
    assert_eq!(row_u16(usize::MAX), u16::MAX);
}

/// `row_u16` is a lossless identity for typical terminal dimensions.
#[test]
fn row_u16_identity_for_typical_values() {
    use super::super::row_u16;

    // Typical terminal rows: 24, 80, 120, 256, 500
    for &val in &[24_usize, 80, 120, 256, 500, 1000, 10_000, 65535] {
        assert_eq!(
            row_u16(val),
            val as u16,
            "row_u16({val}) should be lossless"
        );
    }
}
