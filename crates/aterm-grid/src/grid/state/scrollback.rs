// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scrollback-facing `GridStorage` accessors.

use aterm_scrollback::ScrollbackStorage;

use super::super::{Row, ScrolledRowExtras};
use super::GridStorage;

impl GridStorage {
    /// Attach a scrollback buffer to this grid.
    pub fn attach_scrollback(&mut self, scrollback: impl Into<ScrollbackStorage>) {
        self.scrollback = Some(scrollback.into());
    }

    /// Get a reference to the scrollback storage, if attached.
    #[must_use]
    pub fn scrollback(&self) -> Option<&ScrollbackStorage> {
        self.scrollback.as_ref()
    }

    /// Get a mutable reference to the scrollback storage, if attached.
    pub fn scrollback_mut(&mut self) -> Option<&mut ScrollbackStorage> {
        self.scrollback.as_mut()
    }

    /// Set the scrollback line limit.
    pub fn set_scrollback_line_limit(&mut self, limit: Option<usize>) {
        if let Some(scrollback) = &mut self.scrollback {
            scrollback.set_line_limit(limit);
        }
    }

    /// Get the effective scrollback line limit (`None` = unlimited).
    ///
    /// Tiered grids report the store's limit (also while the store is
    /// temporarily detached for an off-thread reflow, where it reads as
    /// `None`). Ring-only grids have no store — the ring cap IS retention
    /// (see `Grid::set_scrollback_line_limit`) — so the cap is the limit.
    #[must_use]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        if self.scrollback.is_some() || self.scrollback_detached_for_reflow {
            return self
                .scrollback
                .as_ref()
                .and_then(ScrollbackStorage::line_limit);
        }
        (self.max_scrollback < super::UNLIMITED_RING_SCROLLBACK).then_some(self.max_scrollback)
    }

    /// Ring buffer scrollback count (total_lines minus visible).
    #[must_use]
    #[inline]
    pub fn ring_buffer_scrollback(&self) -> usize {
        self.total_lines.saturating_sub(self.visible_rows as usize)
    }

    #[must_use]
    #[allow(
        dead_code,
        reason = "accessor pending scrollback_access delegation (#5804)"
    )]
    pub(crate) fn ring_history_row(&self, ring_idx: usize) -> Option<&Row> {
        if ring_idx >= self.ring_buffer_scrollback() {
            return None;
        }

        debug_assert!(
            !self.rows.is_empty(),
            "get_history_line: ring buffer has zero rows"
        );
        let row_idx = (self.ring_head + ring_idx) % self.rows.len();
        self.rows.get(row_idx)
    }

    #[must_use]
    #[inline]
    #[allow(
        dead_code,
        reason = "accessor pending scrollback_access delegation (#5804)"
    )]
    pub(crate) fn ring_history_extras(&self, ring_idx: usize) -> Option<&ScrolledRowExtras> {
        self.ring_extras
            .get(ring_idx)
            .and_then(|opt| opt.as_deref())
    }

    /// Total scrollback lines (ring buffer + lazy buffer + tiered scrollback).
    #[must_use]
    #[inline]
    pub fn scrollback_lines(&self) -> usize {
        let ring_buffer = self.total_lines.saturating_sub(self.visible_rows as usize);
        let lazy = self.lazy_buffer.len();
        let tiered = self
            .scrollback
            .as_ref()
            .map_or(0, ScrollbackStorage::line_count);
        ring_buffer + lazy + tiered
    }

    /// Number of lines in the lazy buffer (deferred, not yet materialized).
    #[must_use]
    #[inline]
    pub(crate) fn lazy_buffer_lines(&self) -> usize {
        self.lazy_buffer.len()
    }

    /// Lines in the tiered scrollback plus lazy buffer (if any).
    ///
    /// Lazy buffer lines are deferred scrollback lines that have not yet been
    /// materialized. From the caller's perspective, they are scrollback lines
    /// pending promotion to the tiered storage.
    #[must_use]
    #[inline]
    pub fn tiered_scrollback_lines(&self) -> usize {
        let tiered = self
            .scrollback
            .as_ref()
            .map_or(0, ScrollbackStorage::line_count);
        tiered + self.lazy_buffer.len()
    }
}
