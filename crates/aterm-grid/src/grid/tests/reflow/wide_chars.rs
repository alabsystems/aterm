// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Wide-char reflow tests — narrow terminal regressions, spacer handling, cursor clamping.

use crate::grid::reflow::ReflowMode;
use crate::{Cell, CellFlags, Grid, StyleId};

#[test]
fn reflow_narrow_terminal_with_wide_chars_terminates() {
    // Regression test for infinite loop bug (found in 04c19a0b, fixed in c32a95b6)
    // When terminal width is 1-2 columns and content has wide chars,
    // reflow must still make progress and terminate.
    let mut grid = Grid::new(5, 10);

    // Write a wide (CJK) character - occupies 2 cells (WIDE + WIDE_CHAR_SPACER)
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');

    // Resize to 1 column - wide char can't fit, must skip to prevent infinite loop
    // This should complete without hanging
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

    // Resize to 2 columns - wide char was replaced with space, 'A' should survive
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
    assert!(row1.is_empty(), "row 1 should be empty after reflow");
    assert!(
        !row1.is_wrapped(),
        "row 1 should not be wrapped after reflow"
    );
}

#[test]
fn reflow_narrow_terminal_skips_wide_spacer_only_after_replacement() {
    let mut grid = Grid::new(3, 4);

    // Wide char (2 cells) + 2 ASCII cells fits in the row.
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');
    grid.write_char('B');

    // Resize to 1 column - wide char replaced, spacer should be skipped exactly once.
    grid.resize(3, 1);
    grid.assert_invariants();

    let row0_cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(
        row0_cell.char_data(),
        ' ' as u16,
        "wide char should be replaced with space"
    );
    let row1_cell = grid.row(1).unwrap().get(0).unwrap();
    assert_eq!(
        row1_cell.char_data(),
        'A' as u16,
        "first ASCII cell should follow replacement"
    );
    let row2_cell = grid.row(2).unwrap().get(0).unwrap();
    assert_eq!(
        row2_cell.char_data(),
        'B' as u16,
        "second ASCII cell should follow without spacer row"
    );
}

#[test]
fn reflow_narrow_terminal_replaces_multiple_wide_chars() {
    let mut grid = Grid::new(2, 4);

    // Two wide chars back-to-back fill the row.
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

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

#[test]
fn reflow_narrow_terminal_with_wide_chars_clamps_cursor() {
    let mut grid = Grid::new(2, 4);
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');
    grid.set_cursor(0, 1); // Spacer cell position.

    grid.resize(2, 1);
    grid.assert_invariants();

    assert_eq!(grid.cursor_row(), 0, "cursor row should stay on first row");
    assert_eq!(grid.cursor_col(), 0, "cursor col should clamp to 0");
    let cursor_cell = grid
        .row(grid.cursor_row())
        .unwrap()
        .get(grid.cursor_col())
        .unwrap();
    assert_eq!(
        cursor_cell.char_data(),
        ' ' as u16,
        "cursor should land on cleared wide cell replacement"
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
    assert_eq!(
        grid.row(1).unwrap().get(0).unwrap().char_data(),
        'A' as u16,
        "ASCII cell should reflow to the next row after shrink"
    );

    let mut grid = Grid::new(2, 4);
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');
    grid.set_cursor(0, 2); // ASCII cell position.

    grid.resize(2, 1);
    grid.assert_invariants();

    assert_eq!(
        grid.cursor_row(),
        1,
        "cursor row should advance to next row"
    );
    assert_eq!(grid.cursor_col(), 0, "cursor col should clamp to 0");
    let cursor_cell = grid
        .row(grid.cursor_row())
        .unwrap()
        .get(grid.cursor_col())
        .unwrap();
    assert_eq!(
        cursor_cell.char_data(),
        'A' as u16,
        "cursor should land on the reflowed ASCII cell"
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
fn resize_without_reflow_clears_unfittable_wide_chars() {
    let mut grid = Grid::new(2, 4);

    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');

    grid.resize_with_reflow_mode(2, 1, ReflowMode::Disabled);
    grid.assert_invariants();

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert!(
        !cell.is_wide(),
        "wide chars should be cleared when the terminal is too narrow"
    );
}

#[test]
fn resize_no_reflow_clears_wide_char_at_boundary() {
    // Regression test for #1282: Row::resize() must clear wide chars cut at boundary.
    // When shrinking by 1 column, a wide char whose spacer cell gets cut off must
    // be replaced with space to maintain WideCharNotAtEnd invariant.
    let mut grid = Grid::new(2, 4);

    // Write normal chars at positions 0-1
    grid.row_mut(0).unwrap().set(0, Cell::new('A'));
    grid.row_mut(0).unwrap().set(1, Cell::new('B'));

    // Write wide char at positions 2-3 (boundary)
    grid.set_cursor(0, 2);
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    // Resize to 3 columns (no reflow) - cuts the spacer at position 3
    grid.resize_with_reflow_mode(2, 3, ReflowMode::Disabled);
    grid.assert_invariants();

    // The wide char at position 2 should be cleared (not at end anymore)
    let cell = grid.row(0).unwrap().get(2).unwrap();
    assert!(
        cell.is_empty(),
        "wide char at boundary should be replaced with empty cell"
    );

    // Positions 0-1 should be unchanged
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);
    assert_eq!(grid.row(0).unwrap().get(1).unwrap().char_data(), 'B' as u16);
}

/// Reflow shrink: wide char at chunk boundary is deferred to next chunk.
///
/// When shrinking to N columns (N >= 2), if a wide char starts at position
/// chunk_end-1, adjust_chunk_boundary moves the boundary back by 1 so the
/// wide char + spacer stays together in the next chunk. This test verifies
/// the boundary adjustment produces correct content layout and cursor tracking.
///
/// Algorithm audit: exercises adjust_chunk_boundary branch where
/// cells[actual_end - 1].flags().contains(WIDE) is true for non-narrow reflow.
#[test]
fn reflow_shrink_wide_char_at_chunk_boundary() {
    let mut grid = Grid::new(5, 6);

    // Row layout: [A][中][ ][B][C][ ] where 中 occupies cols 1-2
    grid.write_char('A');
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('B');
    grid.write_char('C');

    // Cursor on 'B' at col 3
    grid.set_cursor(0, 3);

    // Shrink to 2 columns. Logical cells: [A, 中, spacer, B, C] (len=5).
    // Chunk 1: cell_offset=0, chunk_end=2. cells[1] is WIDE → actual_end=1.
    //   Result: row 0 = [A] (only 1 cell fits).
    // Chunk 2: cell_offset=1, chunk_end=3. cells[2] is SPACER (not WIDE) →
    //   actual_end=3. Result: row 1 = [中][spacer].
    // Chunk 3: cell_offset=3, chunk_end=5. Result: row 2 = [B][C].
    grid.resize(5, 2);
    grid.assert_invariants();

    // Row 0: 'A'
    assert_eq!(
        grid.row(0).unwrap().get(0).unwrap().char_data(),
        'A' as u16,
        "row 0 col 0 should be 'A'"
    );

    // Row 1: wide char 中 should be intact
    let row1_cell = grid.row(1).unwrap().get(0).unwrap();
    assert!(
        row1_cell.flags().contains(CellFlags::WIDE),
        "row 1 col 0 should be WIDE (wide char deferred from chunk boundary)"
    );

    // Row 2: 'B' and 'C'
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

    // Cursor was at col 3 ('B') → should be on row 2, col 0 after reflow
    assert_eq!(grid.cursor_row(), 2, "cursor should follow 'B' to row 2");
    assert_eq!(grid.cursor_col(), 0, "cursor should be at col 0 on row 2");
}

/// Reflow grow: wide char at last cell of merged content uses cursor clamping.
///
/// When growing and the merged logical line ends with a wide char, the cursor
/// at the last content position should be clamped correctly by finalize_reflow.
///
/// Algorithm audit: exercises cursor_in_grow_chunk where cursor_logical_offset
/// equals actual_end for the last chunk (uses <= comparison, not <).
#[test]
fn reflow_grow_cursor_at_end_of_wide_char_content() {
    let mut grid = Grid::new(4, 4);

    // Row 0: [中][spacer][A][B] — fills 4 columns
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');
    grid.write_char('B');

    // Set up wrapped continuation
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        row.write_char(0, 'C');
    }

    // Cursor at end of continuation row content (col 1 on row 1)
    grid.set_cursor(1, 1);

    // Grow to 10 cols — rows 0+1 merge into "中spacerABC"
    grid.resize(4, 10);
    grid.assert_invariants();

    // Merged content: [中][spacer][A][B][C] on row 0
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

    // Cursor should be on merged row 0, clamped within valid range
    assert_eq!(grid.cursor_row(), 0, "cursor should be on merged row 0");
    assert!(
        grid.cursor_col() < 10,
        "cursor col {} should be within new_cols 10",
        grid.cursor_col()
    );
}
