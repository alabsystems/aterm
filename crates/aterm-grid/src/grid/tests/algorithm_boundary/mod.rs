// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boundary condition, correctness and API-contract tests for grid operations:
//! ED/EL modes with the cursor at edges, line feed / reverse line feed with
//! scroll regions, write_ascii_blast content, carriage return, erase-rect
//! clamping, history line ordering (ring and tiered), complex char overflow,
//! cell extras and damage tracking.

use super::super::*;
use crate::grid::test_support::write_marker_line;

mod erase;
mod history_extras;
mod pending_wrap;
mod resize_and_invariants;
mod scroll_display_damage;
mod scrolling;
mod write_cell_run;
mod write_ops;
mod write_styled;

/// Write a letter to each row at column 0.
fn fill_grid_rows(grid: &mut Grid, rows: u16) {
    for row in 0..rows {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
}

/// Build a 6×5 grid with scroll region [1,3] and content A-F on rows 0-5.
fn build_autowrap_scroll_region_fixture() -> Grid {
    let mut grid = Grid::new(6, 5);
    fill_grid_rows(&mut grid, 6);
    grid.set_scroll_region(1, 3);
    grid
}

/// Assert that rows 0, 4, 5 (outside scroll region [1,3]) are preserved.
fn assert_rows_outside_scroll_region_preserved(grid: &Grid) {
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'E');
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'F');
}
