// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Protected-cell preservation tests — PROTECTED flag survives reflow and narrow resize.

use crate::grid::reflow::ReflowMode;
use crate::{Cell, CellFlags, Grid, StyleId};

#[test]
fn resize_to_one_column_preserves_protected_cells() {
    // Regression test for #1286: PROTECTED cells share bit with WIDE_CHAR_SPACER
    // and were incorrectly dropped during narrow reflow.
    let mut grid = Grid::new(2, 4);

    // Write a protected cell ('P' with PROTECTED flag) at position 0
    let mut protected_cell = Cell::new('P');
    protected_cell.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(0, protected_cell);

    // Write unprotected cells at positions 1-3
    grid.row_mut(0).unwrap().set(1, Cell::new('A'));
    grid.row_mut(0).unwrap().set(2, Cell::new('B'));
    grid.row_mut(0).unwrap().set(3, Cell::new('C'));

    // Resize to 1 column (no reflow)
    grid.resize_with_reflow_mode(2, 1, ReflowMode::Disabled);
    grid.assert_invariants();

    // Protected cell at position 0 should survive
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
    // Reflow path: protected cells should survive when shrinking to 1 column.
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
    // Edge case: protected cell at position 2 (after wide char + spacer).
    // This is the crucial test case because WIDE_CHAR_SPACER and PROTECTED share the
    // same bit (1<<10). The fix ensures that we only skip cells that actually follow
    // a replaced wide char, not any cell with the PROTECTED flag.
    let mut grid = Grid::new(2, 4);

    // Write wide char at position 0-1
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    // Write protected cell at position 2 (immediately after the wide char + spacer)
    let mut protected_cell = Cell::new('P');
    protected_cell.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(2, protected_cell);

    // Write normal cell at position 3
    grid.row_mut(0).unwrap().set(3, Cell::new('X'));

    // Resize to 1 column (triggers reflow path)
    grid.resize(2, 1);
    grid.assert_invariants();

    // The wide char becomes empty at row 0. Protected 'P' should reflow to row 1.
    let row0_cell = grid.row(0).unwrap().get(0).unwrap();
    assert!(
        row0_cell.is_empty(),
        "wide char should be replaced with empty cell"
    );

    // Row 1 is a continuation (wrapped) row
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
    // Ensure narrow reflow skips the spacer cell only for the replaced wide char,
    // so following content is not pushed down an extra row.
    let mut grid = Grid::new(3, 4);

    // Write wide char at position 0-1
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    // Write normal cells after the wide char + spacer
    grid.row_mut(0).unwrap().set(2, Cell::new('A'));
    grid.row_mut(0).unwrap().set(3, Cell::new('B'));

    // Resize to 1 column (triggers reflow path)
    grid.resize(3, 1);
    grid.assert_invariants();

    // Wide char should be replaced with empty cell in row 0.
    let row0_cell = grid.row(0).unwrap().get(0).unwrap();
    assert!(
        row0_cell.is_empty(),
        "wide char should be replaced with empty cell"
    );

    // The spacer should be skipped so 'A' lands on row 1.
    let row1_cell = grid.row(1).unwrap().get(0).unwrap();
    assert_eq!(
        row1_cell.char_data(),
        'A' as u16,
        "spacer should be skipped so 'A' reflows to row 1"
    );

    // 'B' should follow on row 2.
    let row2_cell = grid.row(2).unwrap().get(0).unwrap();
    assert_eq!(
        row2_cell.char_data(),
        'B' as u16,
        "content after the spacer should reflow to the next row"
    );
}

#[test]
fn resize_preserves_multiple_protected_cells_with_wide_chars() {
    // Test multiple protected cells interspersed with wide chars.
    let mut grid = Grid::new(3, 6);

    // Pattern: [P1][中][ ][P2][A ][B ]
    // Where 中 is wide (takes 2 cells), P1/P2 are protected

    // Protected cell at position 0
    let mut p1 = Cell::new('1');
    p1.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(0, p1);

    // Wide char at position 1-2
    grid.set_cursor(0, 1);
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());

    // Protected cell at position 3
    let mut p2 = Cell::new('2');
    p2.set_flags(CellFlags::PROTECTED);
    grid.row_mut(0).unwrap().set(3, p2);

    // Normal cells at positions 4-5
    grid.row_mut(0).unwrap().set(4, Cell::new('A'));
    grid.row_mut(0).unwrap().set(5, Cell::new('B'));

    // Resize to 1 column
    grid.resize(3, 1);
    grid.assert_invariants();

    // Count protected cells - should still be 2
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
