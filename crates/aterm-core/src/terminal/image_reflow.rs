// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keep inline images alive across a width-changing resize.
//!
//! An inline image (OSC 1337 `File=`, and sixel through the same placement) is
//! stamped by `TerminalHandler::place_image` as one
//! [`ImageRef`](aterm_grid::ImageRef) per covered cell in the `CellExtras` side
//! table, with no glyph, so a row carrying only an image has `Row::len() == 0`.
//! Column reflow copies `&row.as_slice()[..row.len()]` and re-keys the extras of
//! exactly the cells it copies, so such a row would lose every `ImageRef`.
//!
//! So before a width-changing reflow each image row's `len` is extended over its
//! image cells, and reflow's own copy carries them wherever it puts the row (an
//! image cell already has `HAS_EXTRAS`, so only `len` needs the nudge). The
//! extension is clamped to the new width: a footprint wider than the window
//! clips instead of wrapping, as `place_image` clips at the right margin.

use aterm_grid::Grid;

impl super::Terminal {
    /// Pin the image rows of the grid about to rewrap (the active grid, or the
    /// saved primary under an alt screen); call before the width change. The
    /// no-reflow path (`resize_no_reflow`) keeps `CellExtras` wholesale.
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

/// Extend every image-bearing row's content length so a column reflow copies its
/// image cells (see the module docs), clamped to `new_cols` so an over-wide
/// footprint clips at the right margin. One pass over the extras map, never
/// `rows * cols` probes.
fn pin_image_rows(grid: &mut Grid, new_cols: u16) {
    // Clamp as the grid does before deciding whether anything rewraps, so a
    // request that lands on the current width leaves the rows untouched (no
    // reflow would consume the lengthened rows).
    if new_cols.clamp(1, crate::grid::MAX_GRID_COLS) == grid.cols() {
        return;
    }
    if grid.extras().is_empty() {
        return;
    }
    let rows = grid.rows();
    let cols = grid.cols();
    // Per-row rightmost image column in one map pass; entries outside the
    // current extent are skipped, as the render path skips them.
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
        // `update_len` only grows, so a row whose text reaches past the image
        // keeps wrapping like the text it is.
        if let Some(row) = grid.row_mut(row_idx) {
            row.update_len(end.min(new_cols));
        }
    }
}
