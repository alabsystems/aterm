// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keep inline images alive across a WIDTH-changing resize.
//!
//! ## The defect this exists to prevent
//!
//! An inline image (iTerm2 OSC 1337 `File=`, and the sixel path that reuses the
//! same placement) is stamped by [`place_image`](super::Terminal::place_image) as
//! one [`ImageRef`](aterm_grid::ImageRef) per covered CELL, living in the grid's
//! `CellExtras` side table. `place_image` writes no glyph — the renderer paints the
//! image OVER the cell — so a row that carries nothing but an image has
//! `Row::len() == 0`.
//!
//! Column reflow copies each source row's cells from `&row.as_slice()[..row.len()]`,
//! and `remap_copied_extra` re-keys the extras of exactly the cells it copies. With
//! `len == 0` an image row copies ZERO cells, so every `ImageRef` on it is dropped
//! on the floor: after any column change the pixels are gone while the rows the
//! footprint occupied remain, leaving a blank hole nothing ever refills. Hyperlinks
//! survived the identical resize because a link always sits on a cell that also
//! carries text, so the row's `len` covers it. That asymmetry — measured live at
//! 0.61.0, `80->79`, `80->81` and a pixel resize all destroying the image while the
//! links on the same screen survived — is the whole bug.
//!
//! ## The fix
//!
//! Before a width-changing reflow, extend each image row's `len` to cover its image
//! cells, so reflow's own cell copy carries them (and their extras) to wherever it
//! puts that row. The image then rides the REAL reflow: no second row-mapping
//! implementation to drift from it, and the placement lands correctly even when the
//! rows above rewrap and shift everything down.
//!
//! An image cell is already non-empty at the `Cell` level — `cell_extra_mut` sets the
//! `HAS_EXTRAS` bit in its packed colors — so the copy propagates `len` onto the
//! destination row by itself; only the SOURCE row's `len` needs the nudge.
//!
//! The extension is clamped to the NEW width, which is what makes a footprint wider
//! than the new window CLIP instead of wrapping: cells past the clamp are not copied,
//! so the tail tiles are dropped rather than re-emitted at column 0 of a row that
//! reflow would have had to invent. Clipping is what `place_image` already documents
//! for a footprint that overruns the right margin, so a resize and a fresh placement
//! agree.
//!
//! ## What this does NOT fix
//!
//! An image that scrolls off the top still dies at the scrollback boundary:
//! `aterm_scrollback::Line` carries hyperlinks but has no image field at all, so
//! there is nowhere for the `ImageRef` to go. That is a storage gap in the history
//! model, not a reflow gap, and it is untouched here.

use aterm_grid::Grid;

impl super::Terminal {
    /// Pin the image rows of the grid that is about to REWRAP, ahead of a width
    /// change. Only that grid needs it: the no-reflow path (`resize_no_reflow`,
    /// used for the app-managed alt screen) resizes rows in place and keeps the
    /// existing `CellExtras` wholesale — its images were never at risk.
    ///
    /// Call BEFORE the resize; afterwards the old width is gone.
    pub(super) fn pin_image_rows_for_width_change(&mut self, new_cols: u16) {
        // Whichever grid holds PRIMARY content is the one that reflows: the
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
    // The grid clamps the requested width (§5.8 ingress bound) before deciding
    // whether anything rewraps; clamp identically, so a request that lands on the
    // CURRENT width after clamping leaves the rows untouched. Without this the
    // lengthened rows would persist with no reflow to consume them.
    if new_cols.clamp(1, crate::grid::MAX_GRID_COLS) == grid.cols() {
        return;
    }
    if grid.extras().is_empty() {
        return;
    }
    let rows = grid.rows();
    let cols = grid.cols();
    // Per-row rightmost image column, collected in ONE map pass. `iter()` yields
    // external coordinates with scrolled-off entries already filtered; entries
    // outside the CURRENT extent are skipped exactly as the render path skips them.
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
        // `update_len` only ever GROWS the length, so a row whose text already
        // reaches past the image is left exactly as it was — the mixed
        // text-plus-image row keeps wrapping like the text it is.
        if let Some(row) = grid.row_mut(row_idx) {
            row.update_len(end.min(new_cols));
        }
    }
}
