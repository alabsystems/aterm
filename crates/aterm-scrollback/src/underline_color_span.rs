// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Underline-colour span type for scrollback lines.
//!
//! Represents a contiguous range of columns that share an SGR 58 underline
//! colour. Used to preserve underline colours when lines scroll from the
//! visible grid into scrollback storage — a sidecar parallel to
//! [`HyperlinkSpan`](super::HyperlinkSpan), keeping the RLE-compressed
//! [`CellAttrs`](super::CellAttrs) wire format unchanged.

/// Underline-colour span within a line.
///
/// Represents a contiguous range of columns that share one SGR 58 underline
/// colour, stored in the **packed** `0xTT_XXXXXX` form so the distinction
/// between an explicit RGB colour (`0x01`) and an indexed palette colour
/// (`0x02`) survives into scrollback. Preserving the index — rather than a
/// resolved RGB triple — lets a scrolled-back indexed underline colour
/// re-resolve against the live palette at render time, exactly as the live
/// cell does (so an OSC 4 palette change still re-colours history).
///
/// ## Memory Layout
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────┐
/// │ start_col: u16 (2 bytes) - Start column (inclusive)             │
/// │ end_col: u16 (2 bytes) - End column (exclusive)                 │
/// │ color: u32 (4 bytes) - Packed underline colour (0xTT_XXXXXX)    │
/// └─────────────────────────────────────────────────────────────────┘
/// Total: 8 bytes per span
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnderlineColorSpan {
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
    /// Packed underline colour (`0xTT_XXXXXX`; `0x01` = RGB, `0x02` = indexed).
    pub color: u32,
}

impl UnderlineColorSpan {
    /// Create a new underline-colour span.
    #[must_use]
    pub const fn new(start_col: u16, end_col: u16, color: u32) -> Self {
        Self {
            start_col,
            end_col,
            color,
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

    /// Get the span width in columns.
    #[inline]
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Serialized size in bytes: fixed `start_col + end_col + color` = 8.
    #[inline]
    #[must_use]
    pub const fn serialized_size(&self) -> usize {
        8
    }
}
