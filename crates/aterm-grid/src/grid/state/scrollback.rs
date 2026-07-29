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

    /// Set the tiered store's RAW line limit (the store SHARE, not the user's
    /// total). The unified total-retention split — total = store share + ring
    /// cap (audit E1) — lives in [`Grid::set_scrollback_line_limit`], which
    /// also drains the lazy buffer and re-caps the ring; this low-level writer
    /// is for callers that already did that arithmetic.
    ///
    /// [`Grid::set_scrollback_line_limit`]: crate::Grid::set_scrollback_line_limit
    pub fn set_store_line_limit(&mut self, limit: Option<usize>) {
        if let Some(scrollback) = &mut self.scrollback {
            scrollback.set_line_limit(limit);
        }
    }

    /// Get the effective scrollback line limit (`None` = unlimited).
    ///
    /// ONE TOTAL retention count (audit E1): tiered grids report the store's
    /// limit PLUS the ring cap — the inverse of the unified split
    /// `Grid::set_scrollback_line_limit` applies — so a set/get round-trips
    /// the same number. During an off-thread reflow the store's detach-time
    /// value (or the newest deferred mutation) remains observable. Ring-only
    /// grids have no store — the ring cap IS retention.
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

    /// True when evicted ring rows must be STAGED for tiered retention
    /// (`scroll_up`'s reuse path): a store is attached and can retain at
    /// least one line, or the store is temporarily detached for an
    /// off-thread reflow (staged lines flush on re-attach — dropping them
    /// would punch a gap in history, audit bug B). A store capped at ZERO
    /// retains nothing — staging every evicted row only for `push_line` to
    /// immediately truncate it would put a per-line materialize+push+drop
    /// cycle on the PTY-drain hot path (the unified-limit `total == ring
    /// cap` default, audit E1), so those rows are discarded directly,
    /// exactly like the no-store path.
    #[must_use]
    #[inline]
    pub(crate) fn stages_evicted_rows(&self) -> bool {
        match &self.scrollback {
            Some(sb) => sb.line_limit() != Some(0),
            None => self.scrollback_detached_for_reflow,
        }
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
