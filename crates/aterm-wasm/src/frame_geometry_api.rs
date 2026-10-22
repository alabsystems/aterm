// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT

//! Shared web admission: clamp grid inputs before allocation, then apply the
//! renderer's framebuffer bound to the grid plus committed message rows.

use aterm_core::grid::{MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_render::Renderer;

use crate::WebTerminal;

#[inline]
pub(crate) fn checked_grid_size(
    renderer: &Renderer,
    rows: u16,
    cols: u16,
    band_rows: usize,
) -> Result<(u16, u16), String> {
    let rows = rows.clamp(1, MAX_GRID_ROWS);
    let cols = cols.clamp(1, MAX_GRID_COLS);
    renderer.checked_frame_size(usize::from(rows) + band_rows, usize::from(cols))?;
    Ok((rows, cols))
}

impl WebTerminal {
    #[inline]
    pub(crate) fn checked_surface_size(&self) -> Result<(usize, usize), String> {
        self.render_face()
            .checked_frame_size(self.rows + self.band.committed(), self.cols)
    }

    #[inline]
    pub(crate) fn checked_surface_geometry(
        &self,
        cell_w: usize,
        cell_h: usize,
        pad: usize,
        head: usize,
    ) -> Result<(usize, usize), String> {
        aterm_render::checked_frame_size_px(
            self.rows + self.band.committed(),
            self.cols,
            cell_w,
            cell_h,
            pad,
            head,
        )
    }
}
