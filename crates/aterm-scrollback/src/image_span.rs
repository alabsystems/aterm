// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inline-image spans for scrollback lines.

use std::sync::Arc;

use aterm_types::{ImageData, KittyPlacementTag};

/// Inline-image span painting footprint row [`Self::image_row`] from [`Self::first_cell_col`] across `[start_col, end_col)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSpan {
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
    /// Zero-indexed row within the image footprint.
    pub image_row: u16,
    /// Footprint column painted by `start_col` (non-zero when clipped on the left).
    pub first_cell_col: u16,
    /// The shared placement payload.
    pub image: Arc<ImageData>,
    /// Kitty placement tag for layer ordering, or `None` for other formats.
    pub kitty: Option<KittyPlacementTag>,
}

impl ImageSpan {
    /// Create a span covering `[start_col, end_col)` at `(image_row, first_cell_col)`.
    #[must_use]
    #[inline]
    pub const fn new(
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
    #[inline]
    #[must_use]
    pub const fn contains(&self, col: u16) -> bool {
        col >= self.start_col && col < self.end_col
    }

    /// Footprint tile `(image_row, image_col)` painted at `col`, or `None` outside the span.
    #[inline]
    #[must_use]
    pub const fn tile_at(&self, col: u16) -> Option<(u16, u16)> {
        if !self.contains(col) {
            return None;
        }
        Some((
            self.image_row,
            self.first_cell_col
                .saturating_add(col.saturating_sub(self.start_col)),
        ))
    }

    /// Bytes this span adds to its line's footprint (`size_of::<Self>()` plus [`ImageData::per_row_bytes`]).
    #[must_use]
    #[inline]
    pub fn memory_used(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.image.per_row_bytes())
    }
}
