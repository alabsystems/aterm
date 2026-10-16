// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Inline-image spans for scrollback lines: a column range painting one row of a
//! single image placement (OSC 1337 `File=`, sixel or Kitty), kept when lines
//! scroll into scrollback, beside [`HyperlinkSpan`](super::HyperlinkSpan) and
//! [`UnderlineColorSpan`](super::UnderlineColorSpan).
//!
//! The live grid stores one `ImageRef` per covered cell; history stores one span
//! per row, since (start column, footprint row, footprint column of the first
//! cell) determine every cell's tile.

use std::sync::Arc;

use aterm_types::{ImageData, KittyPlacementTag};

/// Inline-image span within a line.
///
/// Columns `[start_col, end_col)` paint footprint row [`image_row`] of one
/// placement, left to right from footprint column [`first_cell_col`]. Every row of
/// a footprint, and every rewrapped copy, shares one `Arc` payload, which also
/// keys the renderer's decode cache by pointer identity.
///
/// [`image_row`]: Self::image_row
/// [`first_cell_col`]: Self::first_cell_col
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSpan {
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
    /// Row within the image footprint that every cell of this span paints
    /// (0-indexed from the placement's top-left).
    pub image_row: u16,
    /// Column within the image footprint painted by `start_col`. Normally 0
    /// (a placement's row starts at its own left edge), but non-zero when a
    /// resize or an overwrite left only the right part of a footprint row.
    pub first_cell_col: u16,
    /// The shared placement payload.
    pub image: Arc<ImageData>,
    /// The Kitty placement these cells showed, `None` for any other image. It
    /// orders the image layers of a cell that returns from history as they
    /// were stacked.
    pub kitty: Option<KittyPlacementTag>,
}

impl ImageSpan {
    /// Create a span covering `[start_col, end_col)` with footprint row
    /// `image_row`, whose first cell paints footprint column `first_cell_col`.
    #[must_use]
    pub fn new(
        start_col: u16,
        end_col: u16,
        image_row: u16,
        first_cell_col: u16,
        image: Arc<ImageData>,
    ) -> Self {
        Self {
            start_col,
            end_col,
            image_row,
            first_cell_col,
            image,
            kitty: None,
        }
    }

    /// Check if a column is within this span.
    ///
    /// ENSURES: result == (col >= self.start_col && col < self.end_col)
    #[inline]
    #[must_use]
    pub const fn contains(&self, col: u16) -> bool {
        col >= self.start_col && col < self.end_col
    }

    /// The footprint tile `(image_row, image_col)` that `col` paints, or `None`
    /// when `col` is outside the span. Tiles run left to right from
    /// [`first_cell_col`](Self::first_cell_col), so the whole rectangle is
    /// recovered from the span without storing a ref per cell.
    #[inline]
    #[must_use]
    pub fn tile_at(&self, col: u16) -> Option<(u16, u16)> {
        if !self.contains(col) {
            return None;
        }
        Some((
            self.image_row,
            self.first_cell_col
                .saturating_add(col.saturating_sub(self.start_col)),
        ))
    }

    /// Bytes this span adds to its line's footprint: the struct plus this row's share
    /// of the payload ([`ImageData::per_row_bytes`]).
    ///
    /// Every row of a footprint holds the same `Arc`: charging each row the full
    /// raster would over-report and evict history that is not resident; charging none
    /// would hide images from the budget. The per-row shares sum to the payload once.
    /// Over-counts only when an overwrite split one footprint row into several spans.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.image.per_row_bytes())
    }
}
