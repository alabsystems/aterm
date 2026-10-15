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

    /// Set the tiered store's raw line limit (the store share, not the user's
    /// total). [`Grid::set_scrollback_line_limit`] does the total = share + ring
    /// cap split, drains the lazy buffer and re-caps the ring.
    ///
    /// [`Grid::set_scrollback_line_limit`]: crate::Grid::set_scrollback_line_limit
    #[cfg(test)]
    pub fn set_store_line_limit(&mut self, limit: Option<usize>) {
        if let Some(scrollback) = &mut self.scrollback {
            scrollback.set_line_limit(limit);
        }
    }

    /// The effective total scrollback line limit (`None` = unlimited).
    ///
    /// Tiered grids report the store's limit plus the ring cap, so a set/get
    /// round-trips. During an off-thread reflow the detach-time value (or the
    /// newest deferred mutation) is reported. Ring-only grids report the ring
    /// cap.
    #[must_use]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        if self.scrollback_detached_for_reflow
            && let Some(settings) = self.pending_scrollback_settings
        {
            return settings.line_limit;
        }
        if self.scrollback.is_some() || self.scrollback_detached_for_reflow {
            return self
                .scrollback
                .as_ref()
                .and_then(ScrollbackStorage::line_limit)
                // saturating: an (unrealistic) usize::MAX store limit must not
                // wrap when the ring share is folded back in.
                .map(|store_limit| store_limit.saturating_add(self.max_scrollback));
        }
        (self.max_scrollback < super::UNLIMITED_RING_SCROLLBACK).then_some(self.max_scrollback)
    }

    /// Effective tiered-store byte budget, including a mutation deferred while
    /// the store is detached for off-thread reflow.
    #[must_use]
    pub fn scrollback_memory_budget(&self) -> Option<usize> {
        if self.scrollback_detached_for_reflow
            && let Some(settings) = self.pending_scrollback_settings
        {
            return Some(settings.memory_budget);
        }
        self.scrollback
            .as_ref()
            .map(ScrollbackStorage::memory_budget)
    }

    /// Whether an attached tiered store keeps any line. `false` with no store,
    /// and with a store whose share of the total is zero: a total limit at or
    /// below the ring cap re-caps the ring to the total and leaves the store
    /// nothing (`Grid::set_scrollback_line_limit`). The ring is then the whole
    /// retention, exactly as on a ring-only grid of that cap.
    ///
    /// Every site that gives history a seat asks it: the reflow restore
    /// (`Grid::restore_reflowed_scrollback`) and the offload re-attach
    /// (`Grid::reattach_ring_history`) take the ring route when it is `false`,
    /// and every site where the ring sheds a row asks it through
    /// [`stages_evicted_rows`](Self::stages_evicted_rows).
    #[must_use]
    #[inline]
    pub(crate) fn store_retains_lines(&self) -> bool {
        self.scrollback
            .as_ref()
            .is_some_and(|sb| sb.line_limit() != Some(0))
    }

    /// Whether a row leaving the ring is staged in the lazy buffer rather than
    /// dropped: a store is attached that retains at least one line, or the
    /// store is detached for reflow (staged lines flush on re-attach). With a
    /// zero-share store, rows are discarded directly, as without a store.
    ///
    /// Every site where the ring sheds a row asks this one question: a line
    /// staged with no store to keep it outlives the newer ring rows that later
    /// scrolls drop, leaving a hole in the middle of history. Inside a detach
    /// window (no store attached) the answer is `true` whatever the store's
    /// share: the re-attach seats what was staged, in the ring when the
    /// re-attached store keeps nothing.
    #[must_use]
    #[inline]
    pub(crate) fn stages_evicted_rows(&self) -> bool {
        self.store_retains_lines()
            || (self.scrollback.is_none() && self.scrollback_detached_for_reflow)
    }

    /// Ring buffer scrollback count (total_lines minus visible).
    #[must_use]
    #[inline]
    pub fn ring_buffer_scrollback(&self) -> usize {
        self.total_lines.saturating_sub(self.visible_rows as usize)
    }

    #[must_use]
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
    pub(crate) fn ring_history_extras(&self, ring_idx: usize) -> Option<&ScrolledRowExtras> {
        self.ring_extras.get(ring_idx)
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

    /// Lines in the tiered scrollback plus the lazy buffer (deferred lines
    /// pending promotion to the store).
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
