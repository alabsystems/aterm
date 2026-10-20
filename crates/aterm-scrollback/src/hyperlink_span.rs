// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 8 hyperlink spans, kept when lines scroll from the grid into scrollback.

use std::sync::Arc;

/// A contiguous column range of a line that shares one OSC 8 hyperlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperlinkSpan {
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
    /// URL (shared for efficiency when the same URL spans multiple positions).
    pub url: Arc<str>,
    /// OSC 8 `id=` parameter (shared). `None` if the hyperlink had no explicit ID.
    pub id: Option<Arc<str>>,
}

impl HyperlinkSpan {
    /// Create a new hyperlink span (no explicit ID).
    #[must_use]
    pub fn new(start_col: u16, end_col: u16, url: Arc<str>) -> Self {
        Self {
            start_col,
            end_col,
            url,
            id: None,
        }
    }

    /// Create a new hyperlink span with an explicit OSC 8 ID.
    #[must_use]
    pub fn with_id(start_col: u16, end_col: u16, url: Arc<str>, id: Option<Arc<str>>) -> Self {
        Self {
            start_col,
            end_col,
            url,
            id,
        }
    }

    /// Check if a column is within this span.
    ///
    /// ENSURES: result == (col >= self.start_col && col < self.end_col)
    #[inline]
    #[must_use]
    pub fn contains(&self, col: u16) -> bool {
        col >= self.start_col && col < self.end_col
    }

    /// Get the span width in columns.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn width(&self) -> u16 {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Serialized size in bytes (for memory accounting), as in the v3 record:
    /// start_col (u16) + end_col (u16) + url_len (u32) + url + id_len (u32) + id.
    ///
    /// Saturating only discharges the Trust lane's overflow obligation; in-memory
    /// string lengths always fit.
    #[must_use]
    pub fn serialized_size(&self) -> usize {
        let id_len = match self.id.as_ref() {
            Some(id) => id.len(),
            None => 0,
        };
        12usize
            .saturating_add(self.url.len())
            .saturating_add(id_len)
    }
}
