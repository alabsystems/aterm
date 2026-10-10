// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scroll region and unscroll-from-scrollback tests:
//! - `band_moves`: the row-band record behind `coordinates_invalidated`
//! - `region_ops`: IL/DL/SU/SD within scroll regions
//! - `hyperlink_regressions`: CellExtras/hyperlink coverage
//! - `unscroll`: tiered scrollback unscroll and attribute restoration
//! - `unscroll_region_scroll`: unscroll metadata/damage for cache reuse

use super::super::*;

/// Letter column 0 of every row of `grid` (`A`, `B`, …).
fn lettered(mut grid: Grid) -> Grid {
    for row in 0..grid.rows() {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid
}

/// Column 0 of every visible row, top to bottom.
fn col0(grid: &Grid) -> String {
    (0..grid.rows())
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect()
}

mod band_moves;
mod hyperlink_regressions;
mod region_ops;
mod unscroll;
mod unscroll_region_scroll;
