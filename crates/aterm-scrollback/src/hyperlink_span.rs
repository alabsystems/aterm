// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 8 hyperlink spans, kept when lines scroll from the grid into scrollback.

use aterm_types::HyperlinkData;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// A contiguous column range sharing one OSC 8 value. URL/ID edits detach that
/// value through copy-on-write; changing the range keeps its payload shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperlinkSpan {
    /// Start column (inclusive).
    pub start_col: u16,
    /// End column (exclusive).
    pub end_col: u16,
    /// URL and optional ID, shared with cells and other ranges of the same link.
    pub payload: Arc<HyperlinkData>,
}

impl HyperlinkSpan {
    /// Create a new hyperlink span (no explicit ID).
    #[must_use]
    #[inline]
    pub fn new(start_col: u16, end_col: u16, url: Arc<str>) -> Self {
        Self::with_id(start_col, end_col, url, None)
    }

    /// Create a new hyperlink span with an explicit OSC 8 ID.
    #[must_use]
    #[inline]
    pub fn with_id(start_col: u16, end_col: u16, url: Arc<str>, id: Option<Arc<str>>) -> Self {
        Self::from_shared(start_col, end_col, Arc::new(HyperlinkData { url, id }))
    }

    /// Attach a range to an existing shared hyperlink value.
    #[must_use]
    #[inline]
    pub const fn from_shared(start_col: u16, end_col: u16, payload: Arc<HyperlinkData>) -> Self {
        Self {
            start_col,
            end_col,
            payload,
        }
    }

    /// Give the same hyperlink value a different column range.
    #[must_use]
    #[inline]
    pub fn with_range(&self, start_col: u16, end_col: u16) -> Self {
        Self::from_shared(start_col, end_col, Arc::clone(&self.payload))
    }

    /// Per-reference heap charge for the payload and string bytes. Includes a
    /// modeled two-word Arc header; shared payloads may be charged more than once.
    #[must_use]
    #[inline]
    pub fn retained_payload_bytes(&self) -> usize {
        std::mem::size_of::<HyperlinkData>()
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(self.url.len())
            .saturating_add(self.id.as_ref().map_or(0, |id| id.len()))
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

impl Deref for HyperlinkSpan {
    type Target = HyperlinkData;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.payload
    }
}

impl DerefMut for HyperlinkSpan {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.payload)
    }
}

#[cfg(test)]
#[path = "hyperlink_span_tests.rs"]
mod tests;
