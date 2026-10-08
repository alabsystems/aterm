// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keep inline images alive across a width-changing resize.
//!
//! An inline image (iTerm2 OSC 1337 `File=`, and the sixel path that reuses the
//! same placement) is stamped by [`place_image`](super::Terminal::place_image) as
//! one [`ImageRef`](aterm_grid::ImageRef) per covered cell in the grid's
//! `CellExtras` side table, with no glyph, so a row carrying only an image has
//! `Row::len() == 0`. Column reflow copies `&row.as_slice()[..row.len()]` and
//! re-keys the extras of exactly the cells it copies, so such a row would lose
//! every `ImageRef` on any column change.
//!
//! Before a width-changing reflow, each image row's `len` is extended to cover
//! its image cells, so reflow's own copy carries them (and their extras)
//! wherever it puts the row; there is no second row-mapping to drift. An image
//! cell is already non-empty at the `Cell` level (`cell_extra_mut` sets
//! `HAS_EXTRAS`), so only the source row's `len` needs the nudge.
//!
//! The extension is clamped to the new width, so a footprint wider than the new
//! window clips instead of wrapping, as `place_image` clips a footprint that
//! overruns the right margin.

use aterm_grid::Grid;

impl super::Terminal {
    /// Pin the image rows of the grid that is about to rewrap, ahead of a width
    /// change. Only that grid needs it: the no-reflow path (`resize_no_reflow`,
    /// used for the app-managed alt screen) resizes rows in place and keeps the
    /// existing `CellExtras` wholesale — its images were never at risk.
    ///
    /// Call before the resize; afterwards the old width is gone.
    pub(super) fn pin_image_rows_for_width_change(&mut self, new_cols: u16) {
        // Whichever grid holds primary content is the one that reflows: the
        // active grid normally, the saved primary while an alt screen is up.
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
/// image cells (see the module docs). Clamped to `new_cols` so an over-wide
/// footprint clips at the right margin instead of wrapping onto an invented row.
///
/// Costs one pass over the extras map (which is empty on plain-text screens and
/// otherwise proportional to the extras actually present) plus one `update_len`
/// per image row — never `rows * cols` probes.
fn pin_image_rows(grid: &mut Grid, new_cols: u16) {
    // The grid clamps the requested width (ingress bound) before deciding
    // whether anything rewraps; clamp identically, so a request that lands on the
    // current width after clamping leaves the rows untouched. Without this the
    // lengthened rows would persist with no reflow to consume them.
    if new_cols.clamp(1, crate::grid::MAX_GRID_COLS) == grid.cols() {
        return;
    }
    if grid.extras().is_empty() {
        return;
    }
    let rows = grid.rows();
    let cols = grid.cols();
    // Per-row rightmost image column, collected in one map pass. `iter()` yields
    // external coordinates with scrolled-off entries already filtered; entries
    // outside the current extent are skipped exactly as the render path skips them.
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
        // `update_len` only ever grows the length, so a row whose text already
        // reaches past the image is left exactly as it was — the mixed
        // text-plus-image row keeps wrapping like the text it is.
        if let Some(row) = grid.row_mut(row_idx) {
            row.update_len(end.min(new_cols));
        }
    }
}
