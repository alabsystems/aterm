// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn constructors_clamp_dimensions_and_hold_invariants() {
    let cases = [
        ((0, 80), (1, 80)),
        ((24, 0), (24, 1)),
        ((0, 0), (1, 1)),
        ((u16::MAX, u16::MAX), (MAX_GRID_ROWS, MAX_GRID_COLS)),
        ((10, 40), (10, 40)),
    ];
    for ((rows, cols), want) in cases {
        let tiered = aterm_scrollback::Scrollback::new(50, 500, 500_000);
        for grid in [
            Grid::new(rows, cols),
            Grid::with_scrollback(rows, cols, 500),
            Grid::with_tiered_scrollback(rows, cols, 500, tiered),
        ] {
            assert_eq!((grid.rows(), grid.cols()), want, "{rows}x{cols}");
            grid.assert_invariants();
        }
    }
}
