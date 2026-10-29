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
    #[inline]
    pub const fn new(start_col: u16, end_col: u16, url: Arc<str>) -> Self {
        Self {
            start_col,
            end_col,
            url,
            id: None,
        }
    }

    /// Create a new hyperlink span with an explicit OSC 8 ID.
    #[must_use]
    #[inline]
    pub const fn with_id(
        start_col: u16,
        end_col: u16,
        url: Arc<str>,
        id: Option<Arc<str>>,
    ) -> Self {
        Self {
            start_col,
            end_col,
            url,
            id,
        }
    }

    /// Check if a column is within this span.
    #[inline]
    #[must_use]
    pub const fn contains(&self, col: u16) -> bool {
        col >= self.start_col && col < self.end_col
    }

    /// Serialized size in bytes for memory accounting, as in the v3 record:
    /// start_col (u16) + end_col (u16) + url_len (u32) + url + id_len (u32) + id.
    #[must_use]
    #[inline]
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
