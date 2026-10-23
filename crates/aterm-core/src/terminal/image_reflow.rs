// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keep inline images alive across a width-changing resize.

use aterm_grid::Grid;

impl super::Terminal {
    /// Pin the image rows of the grid about to rewrap before the width change.
    pub(super) fn pin_image_rows_for_width_change(&mut self, new_cols: u16) {
        let reflowing = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        if let Some(grid) = reflowing {
            pin_image_rows(grid, new_cols);
        }
    }
}

fn pin_image_rows(grid: &mut Grid, new_cols: u16) {
    if new_cols.clamp(1, crate::grid::MAX_GRID_COLS) == grid.cols() {
        return;
    }
    if grid.extras().is_empty() {
        return;
    }
    let rows = grid.rows();
    let cols = grid.cols();
    let mut rightmost: Vec<u16> = Vec::new();
    for (coord, extra) in grid.extras().iter() {
        if coord.row >= rows || coord.col >= cols || extra.image().is_none() {
            continue;
        }
        if rightmost.len() <= usize::from(coord.row) {
            rightmost.resize(usize::from(coord.row) + 1, 0);
        }
        let slot = &mut rightmost[usize::from(coord.row)];
        *slot = (*slot).max(coord.col.saturating_add(1));
    }
    for (row_idx, end) in rightmost.into_iter().enumerate() {
        if end == 0 {
            continue;
        }
        let Ok(row_idx) = u16::try_from(row_idx) else {
            continue;
        };
        if let Some(row) = grid.row_mut(row_idx) {
            row.update_len(end.min(new_cols));
        }
    }
}
