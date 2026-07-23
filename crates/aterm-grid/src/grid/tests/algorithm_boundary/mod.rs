// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Algorithm audit: boundary condition, correctness, and API-contract tests.
//!
//! Tests grid-level operations at their boundary conditions:
//! - Erase display modes (ED 0/1/2) with cursor at edges
//! - Erase line modes (EL 0/1/2) with cursor at edges
//! - Line feed / reverse line feed with scroll regions
//! - write_ascii_blast cell content verification
//! - Carriage return correctness
//! - Erase rect boundary clamping
//! - History line API ordering (ring buffer and tiered)
//! - Complex char overflow, cell extras, damage tracking
//!
//! Part of #2128 (grid test coverage) and algorithm_audit phase.

use super::super::*;

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

/// Write a single marker char on a new line.
fn write_marker_line(grid: &mut Grid, marker: char) {
    grid.carriage_return();
    grid.write_char(marker);
    grid.carriage_return();
    grid.line_feed();
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
