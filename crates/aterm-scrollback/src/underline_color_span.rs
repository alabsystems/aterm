// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Underline-colour span type for scrollback lines: a sidecar parallel to
//! [`HyperlinkSpan`](super::HyperlinkSpan) that carries SGR 58 underline
//! colours into history without changing the [`CellAttrs`](super::CellAttrs)
//! wire format.

/// Underline-colour span within a line.
///
/// A contiguous range of columns sharing one SGR 58 underline colour, in the
/// packed `0xTT_XXXXXX` form: an indexed colour (`0x02`) stays an index and
/// re-resolves against the live palette at render time (an OSC 4 change still
/// re-colours history), unlike an explicit RGB colour (`0x01`).
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
}
