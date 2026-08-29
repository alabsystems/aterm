// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Inline-image span type for scrollback lines.
//!
//! Represents a contiguous range of columns painting one row of a single image
//! placement (iTerm2 OSC 1337 `File=`, and the sixel path that reuses the same
//! placement). Used to preserve inline images when lines scroll from the visible
//! grid into scrollback storage — a sidecar parallel to
//! [`HyperlinkSpan`](super::HyperlinkSpan) and
//! [`UnderlineColorSpan`](super::UnderlineColorSpan).
//!
//! ## Why a SPAN and not a per-cell ref
//!
//! One placement covers a `rows`×`cols` RECTANGLE, and the live grid stores it
//! as one `ImageRef` per covered cell — a per-cell copy in history would carry
//! one `Arc` bump, two `u16`s and a heap `Box` for every column of every row, to
//! describe a geometry that is fully determined by (start column, footprint row,
//! footprint column of the first cell). The span says exactly that once per row,
//! and the tile coordinates of the cells in between are arithmetic.

use std::sync::Arc;

use aterm_types::ImageData;

/// Inline-image span within a line.
///
/// The columns `[start_col, end_col)` all paint footprint row [`image_row`] of
/// the SAME placement, left to right, starting at footprint column
/// [`first_cell_col`]. The (possibly large) payload is shared behind the `Arc`:
/// every row of a footprint, and every line the row is copied into by a rewrap,
/// points at ONE allocation, which is also what lets the renderer keep its
/// decode cache keyed by pointer identity.
///
/// [`image_row`]: Self::image_row
/// [`first_cell_col`]: Self::first_cell_col
///
/// ## Memory Layout
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────┐
/// │ start_col: u16 (2 bytes) - Start column (inclusive)             │
/// │ end_col: u16 (2 bytes) - End column (exclusive)                 │
/// │ image_row: u16 (2 bytes) - Footprint row these cells paint      │
/// │ first_cell_col: u16 (2 bytes) - Footprint column of start_col   │
/// │ image: Arc<ImageData> (8 bytes) - SHARED payload                │
/// └─────────────────────────────────────────────────────────────────┘
/// Total: 16 bytes per span, plus a refcount bump on the shared payload
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSpan {
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
    /// Row WITHIN the image footprint that every cell of this span paints
    /// (0-indexed from the placement's top-left).
    pub image_row: u16,
    /// Column within the image footprint painted by `start_col`. Normally 0
    /// (a placement's row starts at its own left edge), but non-zero when a
    /// resize or an overwrite left only the right part of a footprint row.
    pub first_cell_col: u16,
    /// The shared placement payload.
    pub image: Arc<ImageData>,
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

    /// Get the span width in columns.
    #[inline]
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Bytes this span contributes to its LINE's memory footprint: the struct
    /// plus this row's SHARE of the shared payload
    /// ([`ImageData::per_row_bytes`]).
    ///
    /// The share, not the whole payload, is what keeps the scrollback byte
    /// budget honest in both directions. Every row of one footprint holds the
    /// same `Arc`, so charging each of them the full raster would report a
    /// 1 MiB picture as `rows` MiB and make the budget evict history that is not
    /// actually resident; charging none of them would let a wall of images sit
    /// in the hot tier invisible to the budget that exists to bound it. Summing
    /// the per-row share over a whole retained footprint recovers the payload
    /// exactly once, so dropping one line drops exactly its share.
    ///
    /// Over-counts only in the degenerate case where one footprint row was
    /// split into several spans by an overwrite — the safe direction, and the
    /// same convention `HyperlinkSpan`'s URL accounting already takes.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.image.per_row_bytes())
    }
}
