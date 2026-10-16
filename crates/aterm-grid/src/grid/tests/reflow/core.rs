// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Core reflow tests — shrink/grow wrapping, cursor tracking, round-trip, hard breaks.

use super::super::super::reflow::ReflowMode;
use super::super::super::*;

#[test]
fn reflow_shrink_wraps_long_line() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");

    grid.resize(5, 5);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    assert!(grid.row(1).unwrap().is_wrapped());
}

#[test]
fn reflow_grow_unwraps_soft_wrapped_lines() {
    let mut grid = Grid::new(5, 5);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();

    // A soft-wrapped continuation, built by hand.
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
    assert!(grid.row(1).unwrap().is_wrapped());

    grid.resize(5, 12);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");
    assert!(grid.row(1).unwrap().is_empty() || !grid.row(1).unwrap().is_wrapped());
}

#[test]
fn reflow_shrink_preserves_cursor_position() {
    let mut grid = Grid::new(5, 20);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 7);

    // Offset 7 ('H') lands on row 1, col 2.
    grid.resize(5, 5);

    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn reflow_grow_preserves_cursor_position() {
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
    grid.set_cursor(1, 2);

    grid.resize(5, 12);

    // Row 1 col 2 was logical offset 7; unwrapped to 12 cols it is row 0 col 7.
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 7);
}

/// A cursor exactly at a chunk boundary is positioned correctly after reflow.
#[test]
fn reflow_cursor_at_chunk_boundary() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.set_cursor(0, 5);

    grid.resize(5, 5);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    // Offset 5 starts the second chunk: row 1, col 0.
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 0);
}

/// A cursor at the last position of a wrapped line's first segment is
/// positioned correctly after unwrap.
#[test]
fn reflow_cursor_at_wrapped_line_end() {
    let mut grid = Grid::new(5, 5);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    // `is_wrapped` marks a row as the continuation of the previous one.
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    grid.set_cursor(0, 4);

    grid.resize(5, 10);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");

    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 4);
}

#[test]
fn reflow_round_trip() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.resize(5, 5);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    grid.resize(5, 10);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");
    assert!(grid.row(1).unwrap().is_empty());
}

#[test]
fn reflow_without_reflow_flag() {
    // `ReflowMode::Disabled` truncates instead of wrapping.
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.resize_with_reflow_mode(5, 5, ReflowMode::Disabled);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert!(grid.row(1).unwrap().is_empty());
}

#[test]
fn resize_with_reflow_noop_and_grow_preserve_logical_rows_after_scroll() {
    // A no-op or grow resize keeps ring_head unless rows were linearized.
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

    grid.resize_with_reflow_mode(4, 8, ReflowMode::Enabled);
    let after_noop: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(after_noop, before_resize);

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
    // A grow appends new rows after all existing logical rows, even when
    // ring_head != 0.
    let mut grid = Grid::with_scrollback(4, 8, 0);
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Advance ring_head, then make the bottom logical row non-empty.
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
    for c in "Hello".chars() {
        grid.write_char(c);
    }

    grid.resize(5, 3);
    assert_eq!(grid.row(0).unwrap().to_string(), "Hel");
    assert_eq!(grid.row(1).unwrap().to_string(), "lo");
    assert!(grid.row(2).unwrap().is_empty());
}

#[test]
fn reflow_preserves_hard_line_breaks() {
    let mut grid = Grid::new(5, 10);
    for c in "ABC".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    for c in "DEF".chars() {
        grid.write_char(c);
    }

    assert!(!grid.row(0).unwrap().is_wrapped());
    assert!(!grid.row(1).unwrap().is_wrapped());

    grid.resize(5, 20);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABC");
    assert_eq!(grid.row(1).unwrap().to_string(), "DEF");
}
