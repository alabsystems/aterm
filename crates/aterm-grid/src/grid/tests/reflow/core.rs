// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Core reflow tests — shrink/grow wrapping, cursor tracking, round-trip, hard breaks.

use super::super::super::reflow::ReflowMode;
use super::super::super::*;

#[test]
fn reflow_shrink_wraps_long_line() {
    // A line of 10 chars on an 80-col terminal should wrap to 2 rows when
    // the terminal is resized to 5 columns.
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");

    // Resize narrower to 5 columns
    grid.resize(5, 5);

    // Content should reflow to 2 rows
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    // Second row should be marked as wrapped
    assert!(grid.row(1).unwrap().is_wrapped());
}

#[test]
fn reflow_grow_unwraps_soft_wrapped_lines() {
    // Create a grid with a wrapped line
    let mut grid = Grid::new(5, 5);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();

    // Manually set the wrapped flag on row 1 and add more content
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
    assert!(grid.row(1).unwrap().is_wrapped());

    // Resize wider to 12 columns (enough to fit all content on one line)
    grid.resize(5, 12);

    // Content should merge: the wrapped continuation should unwrap
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");
    // Row 1 should now be empty (the continuation merged up)
    assert!(grid.row(1).unwrap().is_empty() || !grid.row(1).unwrap().is_wrapped());
}

#[test]
fn reflow_shrink_preserves_cursor_position() {
    let mut grid = Grid::new(5, 20);
    // Write "ABCDEFGHIJ" and position cursor at column 7 ('H')
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 7); // Position at 'H'

    // Resize to 5 columns - cursor at col 7 should move to row 1, col 2
    grid.resize(5, 5);

    // "ABCDE" on row 0, "FGHIJ" on row 1
    // Original position 7 -> row 1, col 2 (F=0, G=1, H=2)
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn reflow_grow_preserves_cursor_position() {
    // Start with wrapped content
    let mut grid = Grid::new(5, 5);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }
    // Position cursor at row 1, col 2 ('H')
    grid.set_cursor(1, 2);

    // Resize to 12 columns
    grid.resize(5, 12);

    // After unwrap, "ABCDEFGHIJ" is on row 0
    // Position row 1 col 2 was at logical offset 7 (5 from row 0 + 2)
    // After unwrap to 12 cols, it should be row 0 col 7
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 7);
}

/// Test cursor at exact chunk boundary during reflow (#1784).
///
/// When cursor is at the exact end of a row that will become a chunk boundary,
/// verify it ends up correctly positioned after reflow.
#[test]
fn reflow_cursor_at_chunk_boundary() {
    let mut grid = Grid::new(5, 10);
    // Write "ABCDEFGHIJ" - fills exactly row 0 (cols 0-9)
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Position cursor at col 5 (on 'F') - this is the exact boundary
    // when we shrink to 5 columns
    // Note: set_cursor clamps to valid range, so we use col 5 which is valid
    grid.set_cursor(0, 5);

    // Shrink to 5 columns
    // "ABCDE" on row 0 (ends at logical offset 5)
    // "FGHIJ" on row 1 (ends at logical offset 10)
    // Cursor at logical offset 5 is exactly at the chunk boundary
    grid.resize(5, 5);

    // Verify content
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    // Cursor was at col 5 (on 'F'), which is logical offset 5
    // After reflow to 5 cols: this is the exact boundary between chunks
    // Should end up at row 1, col 0 (start of second chunk)
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 0);
}

/// Test cursor at exact end of wrapped line during reflow.
///
/// When a line is wrapped and cursor is at the last valid position of the
/// first segment, verify it ends up correctly positioned after unwrap.
#[test]
fn reflow_cursor_at_wrapped_line_end() {
    // Start with 5-column grid
    let mut grid = Grid::new(5, 5);
    // Write "ABCDE" - fills row 0 exactly (cols 0-4)
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    // Move to row 1 and set up wrapped continuation
    // Note: is_wrapped means "this row is a continuation of the previous row"
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    // Position cursor at row 0, col 4 - the last valid column (on 'E')
    // This is at the exact end of the first segment before the wrap
    grid.set_cursor(0, 4);

    // Grow to 10 columns - should unwrap
    grid.resize(5, 10);

    // "ABCDEFGHIJ" should now be on row 0
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");

    // Cursor was at logical offset 4 (on 'E', the last char of first segment)
    // After reflow, should stay at row 0, col 4
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 4);
}

#[test]
fn reflow_round_trip() {
    // Shrink then grow should preserve content
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Shrink to 5 cols
    grid.resize(5, 5);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    // Grow back to 10 cols
    grid.resize(5, 10);
    // Should merge back
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");
    assert!(grid.row(1).unwrap().is_empty());
}

#[test]
fn reflow_without_reflow_flag() {
    // Test resize_with_reflow_mode(ReflowMode::Disabled) just truncates
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Resize without reflow - should truncate, not wrap
    grid.resize_with_reflow_mode(5, 5, ReflowMode::Disabled);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    // Row 1 should be empty (no wrapping occurred)
    assert!(grid.row(1).unwrap().is_empty());
}

#[test]
fn resize_with_reflow_noop_and_grow_preserve_logical_rows_after_scroll() {
    // Regression for #2184: no-op/grow resize must not reset ring_head unless
    // rows were linearized.
    let mut grid = Grid::with_scrollback(4, 8, 0);
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Advance ring_head away from zero.
    grid.set_cursor(3, 0);
    grid.line_feed();

    let before_resize: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(before_resize, vec!['B', 'C', 'D', ' ']);

    // No-op resize must not disturb logical row order.
    grid.resize_with_reflow_mode(4, 8, ReflowMode::Enabled);
    let after_noop: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(after_noop, before_resize);

    // Grow without trimming must preserve existing rows and append empty rows.
    grid.resize_with_reflow_mode(6, 8, ReflowMode::Enabled);
    let after_grow: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(after_grow, before_resize);
    assert!(grid.row(4).unwrap().is_empty());
    assert!(grid.row(5).unwrap().is_empty());
    grid.assert_invariants();
}

#[test]
fn resize_with_reflow_grow_preserves_nonempty_bottom_row_after_scroll() {
    // Regression for #2184 follow-up: grow must append new rows after all
    // existing logical rows, even when ring_head != 0.
    let mut grid = Grid::with_scrollback(4, 8, 0);
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Advance ring_head away from zero, then make the bottom logical row non-empty.
    grid.set_cursor(3, 0);
    grid.line_feed();
    grid.set_cursor(3, 0);
    grid.write_char('X');

    let before_grow: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(before_grow, vec!['B', 'C', 'D', 'X']);

    grid.resize_with_reflow_mode(6, 8, ReflowMode::Enabled);
    let after_grow: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(after_grow, before_grow);
    assert!(grid.row(4).unwrap().is_empty());
    assert!(grid.row(5).unwrap().is_empty());
    grid.assert_invariants();
}

#[test]
fn reflow_handles_empty_rows() {
    let mut grid = Grid::new(5, 10);
    // Row 0 has content, rows 1-4 are empty
    for c in "Hello".chars() {
        grid.write_char(c);
    }

    // Shrink
    grid.resize(5, 3);
    // "Hel" on row 0, "lo" on row 1
    assert_eq!(grid.row(0).unwrap().to_string(), "Hel");
    assert_eq!(grid.row(1).unwrap().to_string(), "lo");
    // Row 2 should still be empty
    assert!(grid.row(2).unwrap().is_empty());
}

#[test]
fn reflow_preserves_hard_line_breaks() {
    let mut grid = Grid::new(5, 10);
    // Write "ABC" then newline, then "DEF"
    for c in "ABC".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    for c in "DEF".chars() {
        grid.write_char(c);
    }

    // Neither row should be marked as wrapped
    assert!(!grid.row(0).unwrap().is_wrapped());
    assert!(!grid.row(1).unwrap().is_wrapped());

    // Resize wider - hard breaks should be preserved (no unwrapping)
    grid.resize(5, 20);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABC");
    assert_eq!(grid.row(1).unwrap().to_string(), "DEF");
}
