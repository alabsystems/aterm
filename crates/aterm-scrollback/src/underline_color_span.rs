// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Sidecar span carrying SGR 58 underline colours in scrollback lines alongside
//! [`HyperlinkSpan`](super::HyperlinkSpan) without changing [`CellAttrs`](super::CellAttrs).

/// Contiguous column range sharing a packed SGR 58 underline colour (`0xTT_XXXXXX`).
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
    #[inline]
    pub const fn new(start_col: u16, end_col: u16, color: u32) -> Self {
        Self {
            start_col,
            end_col,
            color,
        }
    }

    /// Check if a column is within this span.
    #[inline]
    #[must_use]
    pub const fn contains(&self, col: u16) -> bool {
        col >= self.start_col && col < self.end_col
    }
}
