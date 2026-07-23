// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for write_cell_run — the bulk fill path for repeated characters.

use super::super::super::*;

// write_cell_run content verification

#[test]
fn write_cell_run_content_matches_per_char_write() {
    let mut grid_bulk = Grid::new(4, 10);
    let mut grid_char = Grid::new(4, 10);

    let colors = PackedColors::DEFAULT;
    let flags = CellFlags::empty();
    let mut last_byte = None;

    grid_bulk.write_cell_run(b'A', 10, colors, flags, &mut last_byte);

    for _ in 0..10 {
        grid_char.write_char_wrap('A');
    }

    for col in 0..10 {
        let bulk_cell = grid_bulk.cell(0, col).unwrap();
        let char_cell = grid_char.cell(0, col).unwrap();
        assert_eq!(
            bulk_cell.char(),
            char_cell.char(),
            "cell content mismatch at col {col}"
        );
    }
}

#[test]
fn write_cell_run_fills_exact_line() {
    let mut grid = Grid::new(3, 5);
    let mut last_byte = None;
    let written = grid.write_cell_run(
        b'X',
        5,
        PackedColors::DEFAULT,
        CellFlags::empty(),
        &mut last_byte,
    );
    assert_eq!(written, 5);
    for col in 0..5 {
        assert_eq!(grid.cell(0, col).unwrap().char(), 'X');
    }
    // Cursor should be at last column with pending wrap.
    assert_eq!(grid.cursor_col(), 4);
    assert!(grid.pending_wrap());
    assert_eq!(last_byte, Some(b'X'));
}

#[test]
fn write_cell_run_wraps_across_lines() {
    let mut grid = Grid::new(4, 5);
    let mut last_byte = None;
    let written = grid.write_cell_run(
        b'-',
        12,
        PackedColors::DEFAULT,
        CellFlags::empty(),
        &mut last_byte,
    );
    assert_eq!(written, 12);

    // Row 0: 5x '-'
    for col in 0..5 {
        assert_eq!(grid.cell(0, col).unwrap().char(), '-');
    }
    // Row 1: 5x '-'
    for col in 0..5 {
        assert_eq!(grid.cell(1, col).unwrap().char(), '-');
    }
    // Row 2: 2x '-'
    assert_eq!(grid.cell(2, 0).unwrap().char(), '-');
    assert_eq!(grid.cell(2, 1).unwrap().char(), '-');
    // Rest of row 2 empty
    assert_eq!(grid.cell(2, 2).unwrap().char(), ' ');

    // Cursor on row 2, col 2 (next write position).
    assert_eq!(grid.cursor_row(), 2);
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn write_cell_run_empty_count() {
    let mut grid = Grid::new(3, 5);
    let mut last_byte = None;
    let written = grid.write_cell_run(
        b'Z',
        0,
        PackedColors::DEFAULT,
        CellFlags::empty(),
        &mut last_byte,
    );
    assert_eq!(written, 0);
    assert_eq!(grid.cursor_col(), 0);
    assert!(last_byte.is_none());
}

#[test]
fn write_cell_run_applies_style() {
    let mut grid = Grid::new(3, 10);
    let colors = PackedColors::DEFAULT.set_fg_indexed(1);
    let flags = CellFlags::BOLD;
    let mut last_byte = None;

    grid.write_cell_run(b'#', 5, colors, flags, &mut last_byte);

    for col in 0..5 {
        let cell = grid.cell(0, col).unwrap();
        assert_eq!(cell.char(), '#');
        assert!(cell.flags().contains(CellFlags::BOLD));
    }
}

#[test]
fn write_cell_run_scrolls_at_screen_bottom() {
    let mut grid = Grid::new(3, 5);
    // Position cursor at the last row.
    grid.set_cursor(2, 0);

    let mut last_byte = None;
    // Write 6 chars: fills row 2, wraps to a new row (scrolling row 0 off).
    grid.write_cell_run(
        b'S',
        6,
        PackedColors::DEFAULT,
        CellFlags::empty(),
        &mut last_byte,
    );

    // After writing 5 chars on row 2, it wraps. The grid scrolls up by 1
    // so original rows 1, 2 become rows 0, 1, and the 6th char goes to row 2.
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'S');
    assert_eq!(grid.cursor_row(), 2);
}

#[test]
fn write_cell_run_cursor_consistency_with_write_char_wrap() {
    // Verify cursor ends up in the same place as per-char writes.
    for count in [1, 5, 10, 11, 20] {
        let mut grid_bulk = Grid::new(4, 10);
        let mut grid_char = Grid::new(4, 10);

        let mut last_byte = None;
        grid_bulk.write_cell_run(
            b'Q',
            count,
            PackedColors::DEFAULT,
            CellFlags::empty(),
            &mut last_byte,
        );

        for _ in 0..count {
            grid_char.write_char_wrap('Q');
        }

        assert_eq!(
            grid_bulk.cursor_row(),
            grid_char.cursor_row(),
            "cursor row mismatch for count={count}"
        );
        assert_eq!(
            grid_bulk.cursor_col(),
            grid_char.cursor_col(),
            "cursor col mismatch for count={count}"
        );
        assert_eq!(
            grid_bulk.pending_wrap(),
            grid_char.pending_wrap(),
            "pending_wrap mismatch for count={count}"
        );
    }
}
