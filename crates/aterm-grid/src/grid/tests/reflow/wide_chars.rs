// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Wide-char reflow tests — narrow terminal regressions, spacer handling, cursor clamping.

use crate::grid::reflow::ReflowMode;
use crate::{Cell, CellFlags, Grid};

#[test]
fn reflow_narrow_terminal_with_wide_chars_terminates() {
    let mut grid = Grid::new(5, 10);

    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    grid.write_char('A');

    grid.resize(5, 1);
    grid.assert_invariants();
    let row0 = grid.row(0).unwrap();
    let row1 = grid.row(1).unwrap();
    let row0_cell = row0.get(0).unwrap();
    assert_eq!(
        row0_cell.char_data(),
        ' ' as u16,
        "wide char should be replaced with space at col 0"
    );
    assert!(
        !row0_cell.flags().contains(CellFlags::WIDE),
        "wide char replacement should not retain wide flag"
    );
    assert!(
        !row0_cell.flags().contains(CellFlags::WIDE_CONTINUATION),
        "wide char replacement should not retain spacer flag"
    );
    let row1_cell = row1.get(0).unwrap();
    assert_eq!(
        row1_cell.char_data(),
        'A' as u16,
        "ASCII cell should reflow to the next row at col 0"
    );
    assert!(
        !row1_cell.flags().contains(CellFlags::WIDE),
        "ASCII cell should not carry wide flag"
    );
    assert!(
        !row1_cell.flags().contains(CellFlags::WIDE_CONTINUATION),
        "ASCII cell should not carry spacer flag"
    );
    assert!(
        !row0.is_wrapped(),
        "row 0 should not be wrapped after shrink"
    );
    assert!(row1.is_wrapped(), "row 1 should be wrapped after shrink");

    grid.resize(5, 2);
    grid.assert_invariants();
    let row0 = grid.row(0).unwrap();
    let row0_cell = row0.get(0).unwrap();
    assert_eq!(
        row0_cell.char_data(),
        ' ' as u16,
        "wide char should remain replaced with space"
    );
    assert!(
        !row0_cell.flags().contains(CellFlags::WIDE),
        "wide char replacement should not retain wide flag"
    );
    assert!(
        !row0_cell.flags().contains(CellFlags::WIDE_CONTINUATION),
        "wide char replacement should not retain spacer flag"
    );
    let row0_a = row0.get(1).unwrap();
    assert_eq!(
        row0_a.char_data(),
        'A' as u16,
        "ASCII cell should survive reflow back to 2 columns"
    );
    assert!(
        !row0_a.flags().contains(CellFlags::WIDE),
        "ASCII cell should not carry wide flag"
    );
    assert!(
        !row0_a.flags().contains(CellFlags::WIDE_CONTINUATION),
        "ASCII cell should not carry spacer flag"
    );
    let row1 = grid.row(1).unwrap();
    assert!(
        !row0.is_wrapped(),
        "row 0 should not be wrapped after reflow"
    );
    // The cursor after the A fills nothing: it continues the line on a row of its own, where the
    // next glyph goes.
    assert!(row1.is_empty(), "row 1 should be empty after reflow");
    assert!(row1.is_wrapped(), "row 1 holds the cursor after the A");
    assert_eq!((grid.cursor_row(), grid.cursor_col()), (1, 0));
    assert!(grid.row(2).unwrap().is_empty() && !grid.row(2).unwrap().is_wrapped());
}

#[test]
fn reflow_narrow_terminal_replaces_multiple_wide_chars() {
    let mut grid = Grid::new(2, 4);

    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );

    grid.resize(2, 1);
    grid.assert_invariants();

    for row_idx in 0..2 {
        let cell = grid.row(row_idx).unwrap().get(0).unwrap();
        assert_eq!(
            cell.char_data(),
            ' ' as u16,
            "wide char should be replaced with space"
        );
        assert!(
            !cell.flags().contains(CellFlags::WIDE),
            "replacement should not retain wide flag"
        );
        assert!(
            !cell.flags().contains(CellFlags::WIDE_CONTINUATION),
            "replacement should not retain spacer flag"
        );
    }
}

fn shrink_wide_pair_with_cursor_at(col: u16, row: u16, ch: char, what: &str) -> Grid {
    let mut grid = Grid::new(2, 4);
    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    grid.write_char('A');
    grid.set_cursor(0, col);

    grid.resize(2, 1);
    grid.assert_invariants();

    assert_eq!(grid.cursor_row(), row, "cursor row after the shrink");
    assert_eq!(grid.cursor_col(), 0, "cursor col should clamp to 0");
    let cursor_cell = grid
        .row(grid.cursor_row())
        .unwrap()
        .get(grid.cursor_col())
        .unwrap();
    assert_eq!(
        cursor_cell.char_data(),
        ch as u16,
        "cursor should land on {what}"
    );
    assert!(
        !cursor_cell.flags().contains(CellFlags::WIDE),
        "cursor cell should not carry wide flag"
    );
    assert!(
        !cursor_cell.flags().contains(CellFlags::WIDE_CONTINUATION),
        "cursor cell should not carry spacer flag"
    );
    let row0 = grid.row(0).unwrap();
    let row1 = grid.row(1).unwrap();
    assert!(
        !row0.is_wrapped(),
        "row 0 should not be wrapped after shrink"
    );
    assert!(row1.is_wrapped(), "row 1 should be wrapped after shrink");
    grid
}

#[test]
fn reflow_narrow_terminal_with_wide_chars_clamps_cursor() {
    let grid = shrink_wide_pair_with_cursor_at(1, 0, ' ', "cleared wide cell replacement");
    assert_eq!(
        grid.row(1).unwrap().get(0).unwrap().char_data(),
        'A' as u16,
        "ASCII cell should reflow to the next row after shrink"
    );

    let grid = shrink_wide_pair_with_cursor_at(2, 1, 'A', "the reflowed ASCII cell");
    let row0_cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(
        row0_cell.char_data(),
        ' ' as u16,
        "wide char should be replaced with space after shrink"
    );
    assert!(
        !row0_cell.flags().contains(CellFlags::WIDE),
        "wide char replacement should not retain wide flag"
    );
    assert!(
        !row0_cell.flags().contains(CellFlags::WIDE_CONTINUATION),
        "wide char replacement should not retain spacer flag"
    );
}

#[test]
fn resize_no_reflow_clears_wide_char_at_boundary() {
    let mut grid = Grid::new(2, 4);

    grid.row_mut(0).unwrap().set(0, Cell::new('A'));
    grid.row_mut(0).unwrap().set(1, Cell::new('B'));

    grid.set_cursor(0, 2);
    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );

    grid.resize_with_reflow_mode(2, 3, ReflowMode::Disabled);
    grid.assert_invariants();

    let cell = grid.row(0).unwrap().get(2).unwrap();
    assert!(
        cell.is_empty(),
        "wide char at boundary should be replaced with empty cell"
    );

    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);
    assert_eq!(grid.row(0).unwrap().get(1).unwrap().char_data(), 'B' as u16);
}

#[test]
fn reflow_shrink_wide_char_at_chunk_boundary() {
    let mut grid = Grid::new(5, 6);

    grid.write_char('A');
    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    grid.write_char('B');
    grid.write_char('C');

    grid.set_cursor(0, 3);

    grid.resize(5, 2);
    grid.assert_invariants();

    assert_eq!(
        grid.row(0).unwrap().get(0).unwrap().char_data(),
        'A' as u16,
        "row 0 col 0 should be 'A'"
    );

    let row1_cell = grid.row(1).unwrap().get(0).unwrap();
    assert!(
        row1_cell.flags().contains(CellFlags::WIDE),
        "row 1 col 0 should be WIDE (wide char deferred from chunk boundary)"
    );

    assert_eq!(
        grid.row(2).unwrap().get(0).unwrap().char_data(),
        'B' as u16,
        "row 2 col 0 should be 'B'"
    );
    assert_eq!(
        grid.row(2).unwrap().get(1).unwrap().char_data(),
        'C' as u16,
        "row 2 col 1 should be 'C'"
    );

    assert_eq!(grid.cursor_row(), 2, "cursor should follow 'B' to row 2");
    assert_eq!(grid.cursor_col(), 0, "cursor should be at col 0 on row 2");
}

#[test]
fn reflow_grow_cursor_at_end_of_wide_char_content() {
    let mut grid = Grid::new(4, 4);

    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    grid.write_char('A');
    grid.write_char('B');

    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        row.write_char(0, 'C');
    }

    grid.set_cursor(1, 1);

    grid.resize(4, 10);
    grid.assert_invariants();

    assert!(
        grid.row(0).unwrap().get(0).unwrap().is_wide(),
        "merged row should start with wide char"
    );
    assert_eq!(
        grid.row(0).unwrap().get(2).unwrap().char_data(),
        'A' as u16,
        "merged row col 2 should be 'A'"
    );
    assert_eq!(
        grid.row(0).unwrap().get(4).unwrap().char_data(),
        'C' as u16,
        "merged row col 4 should be 'C'"
    );

    assert_eq!(grid.cursor_row(), 0, "cursor should be on merged row 0");
    assert!(
        grid.cursor_col() < 10,
        "cursor col {} should be within new_cols 10",
        grid.cursor_col()
    );
}
