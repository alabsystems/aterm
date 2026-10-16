// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The PROTECTED flag survives reflow and narrow resize.

use crate::grid::reflow::ReflowMode;
use crate::{Cell, CellFlags, Grid, StyleId};

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
    // A protected cell right after a wide char + spacer: only cells following a
    // replaced wide char are skipped, not every cell with bit 10 set.
    let mut grid = Grid::new(2, 4);

    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    let mut protected_cell = Cell::new('P');
    protected_cell.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(2, protected_cell);

    grid.row_mut(0).unwrap().set(3, Cell::new('X'));

    grid.resize(2, 1);
    grid.assert_invariants();

    // The wide char cannot fit one column and is blanked; 'P' reflows to row 1.
    let row0_cell = grid.row(0).unwrap().get(0).unwrap();
    assert!(
        row0_cell.is_empty(),
        "wide char should be replaced with empty cell"
    );

    let row1 = grid.row(1).unwrap();
    assert!(
        row1.is_wrapped(),
        "row 1 should be marked as wrapped continuation"
    );
    let row1_cell = row1.get(0).unwrap();
    assert_eq!(
        row1_cell.char_data(),
        'P' as u16,
        "protected cell should reflow to row 1"
    );
    assert!(
        row1_cell.flags().contains(CellFlags::PROTECTED),
        "protected flag should survive reflow after wide char"
    );
}

#[test]
fn resize_to_one_column_skips_wide_char_spacer() {
    // Only the replaced wide char's spacer is skipped, so following content is not pushed down
    // an extra row.
    let mut grid = Grid::new(3, 4);

    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    grid.row_mut(0).unwrap().set(2, Cell::new('A'));
    grid.row_mut(0).unwrap().set(3, Cell::new('B'));

    grid.resize(3, 1);
    grid.assert_invariants();

    let row0_cell = grid.row(0).unwrap().get(0).unwrap();
    assert!(
        row0_cell.is_empty(),
        "wide char should be replaced with empty cell"
    );

    let row1_cell = grid.row(1).unwrap().get(0).unwrap();
    assert_eq!(
        row1_cell.char_data(),
        'A' as u16,
        "spacer should be skipped so 'A' reflows to row 1"
    );

    let row2_cell = grid.row(2).unwrap().get(0).unwrap();
    assert_eq!(
        row2_cell.char_data(),
        'B' as u16,
        "content after the spacer should reflow to the next row"
    );
}

#[test]
fn resize_preserves_multiple_protected_cells_with_wide_chars() {
    let mut grid = Grid::new(3, 6);

    // [1][中 中][2][A][B], '1' and '2' protected.

    let mut p1 = Cell::new('1');
    p1.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(0, p1);

    grid.set_cursor(0, 1);
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    let mut p2 = Cell::new('2');
    p2.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(3, p2);

    grid.row_mut(0).unwrap().set(4, Cell::new('A'));
    grid.row_mut(0).unwrap().set(5, Cell::new('B'));

    grid.resize(3, 1);
    grid.assert_invariants();

    let mut protected_count = 0;
    for row_idx in 0..3 {
        if let Some(row) = grid.row(row_idx) {
            let cell = row.get(0).unwrap();
            if cell.flags().contains(CellFlags::PROTECTED) {
                protected_count += 1;
            }
        }
    }
    assert_eq!(
        protected_count, 2,
        "both protected cells should survive reflow"
    );
}
