// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The PROTECTED flag survives reflow and narrow resize.

use crate::grid::reflow::ReflowMode;
use crate::{Cell, CellFlags, Grid};

#[test]
fn resize_to_one_column_preserves_protected_cells() {
    // PROTECTED shares bit 10 with the wide spacer; it survives a narrow resize.
    let mut grid = Grid::new(2, 4);

    let mut protected_cell = Cell::new('P');
    protected_cell.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(0, protected_cell);

    grid.row_mut(0).unwrap().set(1, Cell::new('A'));
    grid.row_mut(0).unwrap().set(2, Cell::new('B'));
    grid.row_mut(0).unwrap().set(3, Cell::new('C'));

    grid.resize_with_reflow_mode(2, 1, ReflowMode::Disabled);
    grid.assert_invariants();

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(
        cell.char_data(),
        'P' as u16,
        "protected cell content should survive"
    );
    assert!(
        cell.flags().contains(CellFlags::PROTECTED),
        "protected flag should be preserved"
    );
}

#[test]
fn resize_to_one_column_preserves_protected_cells_with_reflow() {
    let mut grid = Grid::new(4, 4);

    let mut protected_cell = Cell::new('P');
    protected_cell.set_flags(CellFlags::PROTECTED);

    grid.row_mut(0).unwrap().set(0, Cell::new('A'));
    grid.row_mut(0).unwrap().set(1, protected_cell);
    grid.row_mut(0).unwrap().set(2, Cell::new('B'));
    grid.row_mut(0).unwrap().set(3, Cell::new('C'));

    grid.resize(4, 1);
    grid.assert_invariants();

    let mut found = false;
    for row_idx in 0..grid.rows() {
        let cell = grid.row(row_idx).unwrap().get(0).unwrap();
        if cell.char_data() == 'P' as u16 {
            assert!(
                cell.flags().contains(CellFlags::PROTECTED),
                "protected flag should survive reflow"
            );
            found = true;
            break;
        }
    }

    assert!(
        found,
        "protected cell content should survive reflow to 1 column"
    );
}

#[test]
fn resize_to_one_column_preserves_protected_cell_after_wide_char() {
    // PROTECTED shares bit 10 with WIDE_CONTINUATION: only the cell after a
    // replaced wide char is skipped, not every cell with bit 10 set.
    let mut grid = Grid::new(2, 4);

    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );

    let mut protected_cell = Cell::new('P');
    protected_cell.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(2, protected_cell);

    grid.row_mut(0).unwrap().set(3, Cell::new('X'));

    grid.resize(2, 1);
    grid.assert_invariants();

    // The wide char cannot fit one column and is blanked; 'P' reflows to the
    // next row. The line takes three rows at one column: the rewrap keeps its
    // end on the screen, the cursor's row ('P') on top, and its blanked head
    // goes to history.
    let head = grid
        .get_history_line(grid.scrollback_lines() - 1)
        .expect("the line's head is the newest history line");
    assert!(
        head.to_string().trim().is_empty(),
        "wide char should be replaced with empty cell"
    );

    let row0 = grid.row(0).unwrap();
    assert!(
        row0.is_wrapped(),
        "row 0 should be marked as wrapped continuation"
    );
    let row0_cell = row0.get(0).unwrap();
    assert_eq!(
        row0_cell.char_data(),
        'P' as u16,
        "protected cell should reflow to the row after the wide char"
    );
    assert!(
        row0_cell.flags().contains(CellFlags::PROTECTED),
        "protected flag should survive reflow after wide char"
    );
    assert_eq!(grid.row(1).unwrap().get(0).unwrap().char_data(), 'X' as u16);
}
