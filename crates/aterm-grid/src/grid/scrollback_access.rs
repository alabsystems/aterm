// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scrollback query and management methods for [`Grid`].
//!
//! Provides access to tiered scrollback storage, ring buffer scrollback,
//! and unified history line retrieval. Complements the scroll operations
//! in [`super::scroll`] and materialization in [`super::scroll_materialize`].

use std::borrow::Cow;

use aterm_scrollback::ScrollbackError;
use aterm_scrollback::{Line, ScrollbackStorage};

use super::{Grid, scroll_convert, scroll_materialize};

impl Grid {
    // -------------------------------------------------------------------------
    // Public forwarding stubs (bodies live on GridStorage)
    // -------------------------------------------------------------------------

    /// Get a mutable reference to the scrollback storage, if attached.
    ///
    /// Drains the lazy buffer first so all deferred lines are available
    /// in the tiered scrollback.
    pub fn scrollback_mut(&mut self) -> Option<&mut ScrollbackStorage> {
        self.drain_lazy_buffer();
        self.storage.scrollback_mut()
    }

    /// Get total scrollback lines available (ring buffer + tiered scrollback).
    #[must_use]
    #[inline]
    pub fn scrollback_lines(&self) -> usize {
        self.storage.scrollback_lines()
    }

    /// Get the number of lines in the tiered scrollback (if any).
    #[must_use]
    #[inline]
    pub fn tiered_scrollback_lines(&self) -> usize {
        self.storage.tiered_scrollback_lines()
    }

    // -------------------------------------------------------------------------
    // History line access
    // -------------------------------------------------------------------------

    /// Get a historical line by index (0 = oldest).
    ///
    /// This method provides unified access to all scrollback history:
    /// 1. Lines from the tiered scrollback (oldest, if any)
    /// 2. Lines from the lazy buffer (deferred, between tiered and ring)
    /// 3. Lines from the ring buffer scrollback (newest)
    ///
    /// Returns `Ok(None)` if the index is out of bounds.
    ///
    /// Takes `&self` because the disk-backed scrollback's LRU cache and
    /// deferred line `OnceCell` both use interior mutability.
    pub fn try_get_history_line(
        &self,
        idx: usize,
    ) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        // Actual tiered scrollback lines (not including lazy buffer).
        let actual_tiered = self
            .storage
            .scrollback
            .as_ref()
            .map_or(0, |sb| sb.line_count());
        let lazy_count = self.storage.lazy_buffer_lines();
        let ring_count = self.storage.ring_buffer_scrollback();

        if idx >= actual_tiered + lazy_count + ring_count {
            return Ok(None);
        }

        if idx < actual_tiered {
            // Line is in tiered scrollback (Cow::Borrowed for hot, Cow::Owned for warm/cold)
            let Some(scrollback) = self.storage.scrollback() else {
                return Ok(None);
            };
            scrollback.get_line(idx)
        } else if idx < actual_tiered + lazy_count {
            // Line is in the lazy buffer (deferred — materializes on first access)
            let lazy_idx = idx - actual_tiered;
            match self.storage.lazy_buffer.get_line(lazy_idx) {
                Some(line) => Ok(Some(Cow::Borrowed(line))),
                None => Ok(None),
            }
        } else {
            // Line is in ring buffer scrollback (always owned — constructed from row)
            let ring_idx = idx - actual_tiered - lazy_count;
            let Some(row) = self.storage.ring_history_row(ring_idx) else {
                return Ok(None);
            };
            // Use preserved extras from ring_extras (#4149, #4215).
            let default_extras = scroll_convert::ScrolledRowExtras::default();
            let extras = self
                .storage
                .ring_history_extras(ring_idx)
                .unwrap_or(&default_extras);
            Ok(Some(Cow::Owned(Self::row_to_line_with_stored_extras(
                row, extras,
            ))))
        }
    }

    /// Get a historical line by index (0 = oldest), logging read failures.
    #[must_use]
    pub fn get_history_line(&self, idx: usize) -> Option<Cow<'_, Line>> {
        match self.try_get_history_line(idx) {
            Ok(line) => line,
            Err(error) => {
                aterm_log::warn!("get_history_line({idx}) failed: {error}");
                None
            }
        }
    }

    /// A historical line by reverse index (0 = most recent scrollback line).
    ///
    /// This is useful for displaying scrollback from bottom to top.
    pub(crate) fn try_history_line_rev(
        &self,
        rev_idx: usize,
    ) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        let total = self.storage.scrollback_lines();
        if rev_idx >= total {
            return Ok(None);
        }
        self.try_get_history_line(total - 1 - rev_idx)
    }

    /// Get a historical line by reverse index (0 = most recent), logging read failures.
    ///
    /// Returns `Cow::Borrowed` for hot-tier lines (zero-copy render path)
    /// and `Cow::Owned` for warm/cold/ring-buffer lines.
    #[must_use]
    pub fn history_line_rev(&self, rev_idx: usize) -> Option<Cow<'_, Line>> {
        match self.try_history_line_rev(rev_idx) {
            Ok(line) => line,
            Err(error) => {
                aterm_log::warn!("history_line_rev({rev_idx}) failed: {error}");
                None
            }
        }
    }

    /// Drop the `n` oldest ring-scrollback lines (ring-only retention shrink;
    /// see [`Grid::set_scrollback_line_limit`]).
    ///
    /// Rebuilds the retained rows into a fresh page store (the
    /// `erase_scrollback` pattern) so the evicted lines' page memory is
    /// actually reclaimed — a plain drain would strand it in the old pages
    /// until the next full rebuild. Surviving lines keep their absolute-row
    /// identity: only `total_lines` shrinks, so `oldest_absolute_row()`
    /// advances by exactly `n` — the same observable effect as `scroll_up`'s
    /// at-capacity eviction.
    pub(crate) fn evict_oldest_ring_scrollback(&mut self, n: usize) {
        let n = n.min(self.storage.ring_buffer_scrollback());
        if n == 0 {
            return;
        }
        debug_assert!(
            !self.storage.rows.is_empty(),
            "evict_oldest_ring_scrollback: ring buffer has zero rows"
        );
        let keep = self.storage.total_lines - n;
        let start = (self.storage.ring_head + n) % self.storage.rows.len();
        let mut new_pages = super::PageStore::new();
        let mut new_rows = Vec::with_capacity(keep);
        for i in 0..keep {
            let idx = (start + i) % self.storage.rows.len();
            // SAFETY: `new_rows` and `new_pages` are installed into `self`
            // together below, so rows never outlive their backing store.
            let mut row = unsafe { super::Row::new(self.storage.cols, &mut new_pages) };
            row.copy_from(&self.storage.rows[idx]);
            new_rows.push(row);
        }
        self.storage.rows = new_rows;
        self.storage.pages = new_pages;
        self.storage.total_lines = keep;
        self.storage.ring_head = 0;
        // ring_extras parallels ring history (index 0 = oldest); it may be
        // legitimately empty (the post-clear steady-state fast path).
        for _ in 0..n {
            self.storage.ring_extras.pop_front();
        }
        self.storage.generations.evict_all();
        // The retained line set changed: re-clamp a scrolled-back viewport and
        // invalidate content-keyed caches (search index, cross-session polls).
        self.clamp_display_offset();
        self.storage.mark_content_full();
    }

    /// Materialize a scrollback line with full fidelity (#4216).
    ///
    /// Returns a [`MaterializedRow`](scroll_materialize::MaterializedRow)
    /// containing cells plus supplementary extras (hyperlinks, complex chars,
    /// RGB colors). The bridge renderer queries
    /// `MaterializedRow::get_extra(col)` for scrollback cells using the same
    /// code path as visible-area cells.
    ///
    /// For hot-tier lines, the `Cow` from `history_line_rev` avoids cloning —
    /// `materialize_from_line` only needs `&Line` via `Deref`. (#5950)
    #[must_use]
    pub fn materialize_scrollback_row_full(
        &self,
        rev_idx: usize,
        cols: u16,
    ) -> Option<scroll_materialize::MaterializedRow> {
        let line = self.history_line_rev(rev_idx)?;
        Some(scroll_materialize::materialize_from_line(&line, cols))
    }

    /// Get total history line count (tiered + ring buffer scrollback).
    #[must_use]
    #[inline]
    pub fn history_line_count(&self) -> usize {
        self.storage.scrollback_lines()
    }

    // -------------------------------------------------------------------------
    // Memory budget
    // -------------------------------------------------------------------------

    /// Enable memory-bounded scrollback with disk spill.
    ///
    /// When the in-memory scrollback exceeds the configured budget, oldest
    /// cold-tier lines are evicted to a memory-mapped temp file. The temp
    /// file is cleaned up when the grid is dropped.
    // Disk cold-tier only; absent on wasm (no libc/zstd-sys).
    #[cfg(feature = "disk-tier")]
    pub fn set_scrollback_budget(&mut self, budget: super::scrollback_budget::ScrollbackBudget) {
        self.storage.budget_enforcer = Some(super::scrollback_budget::BudgetEnforcer::new(budget));
    }

    /// Query current scrollback memory usage and budget statistics.
    ///
    /// Returns `None` if no budget is configured.
    // Disk cold-tier only; absent on wasm (no libc/zstd-sys).
    #[cfg(feature = "disk-tier")]
    #[must_use]
    pub fn scrollback_memory_stats(
        &self,
    ) -> Option<super::scrollback_budget::ScrollbackMemoryStats> {
        self.storage
            .budget_enforcer
            .as_ref()
            .map(|e| e.memory_stats(self.storage.scrollback.as_ref()))
    }

    /// Get a line that was spilled to disk by the budget enforcer.
    ///
    /// Index 0 is the oldest spilled line. Returns `None` if no budget is
    /// configured or the index is out of bounds.
    // Disk cold-tier only; absent on wasm (no libc/zstd-sys).
    #[cfg(feature = "disk-tier")]
    #[must_use]
    pub fn get_spilled_line(&self, idx: usize) -> Option<Line> {
        self.storage
            .budget_enforcer
            .as_ref()
            .and_then(|e| e.get_spilled_line(idx))
    }
}

// Uses aterm-scrollback's disk-tier-gated DiskBackedScrollback APIs.
#[cfg(all(test, feature = "disk-tier"))]
mod tests {
    use super::*;
    use aterm_scrollback::{
        DiskBackedScrollback, DiskBackedScrollbackConfig, Line, ScrollbackStorage,
    };

    /// Create a grid with corrupted disk-backed scrollback for error-path tests.
    fn grid_with_corrupted_scrollback() -> (Grid, aterm_tempfile::TempDir) {
        let temp_dir = aterm_tempfile::tempdir().expect("create temp dir");
        let cold_path = temp_dir.path().join("scrollback.dtrm");
        let config = DiskBackedScrollbackConfig::new(&cold_path)
            .with_hot_limit(0)
            .with_warm_limit(0)
            .with_block_size(1);
        let disk = DiskBackedScrollback::with_config(config).expect("create disk scrollback");
        let mut storage: ScrollbackStorage = disk.into();

        for i in 0..10 {
            storage
                .push_line(Line::from(format!("Line{i:02}").as_str()))
                .expect("push line");
        }

        // Truncate the cold file to corrupt disk-backed pages.
        #[cfg(unix)]
        std::fs::OpenOptions::new()
            .write(true)
            .open(&cold_path)
            .and_then(|file| file.set_len(32))
            .expect("truncate cold file");
        // Windows twin: the cold tier holds the file MAPPED, and Windows refuses
        // to shrink a user-mapped file (ERROR_USER_MAPPED_FILE), so corrupt by
        // OVERWRITING every page byte past the 32-byte header with garbage
        // instead — same observable effect (every cold-page read fails).
        #[cfg(windows)]
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&cold_path)
                .expect("open cold file");
            let len = file.metadata().expect("stat cold file").len();
            file.seek(SeekFrom::Start(32)).expect("seek past header");
            file.write_all(&vec![
                0xFF;
                usize::try_from(len.saturating_sub(32)).unwrap()
            ])
            .expect("overwrite cold pages");
        }

        let mut grid = Grid::new(5, 80);
        grid.attach_scrollback(storage);
        (grid, temp_dir)
    }

    /// Regression (#4496): `try_get_history_line` propagates scrollback errors
    /// instead of masking them as `None`.
    #[test]
    fn try_get_history_line_propagates_scrollback_error() {
        let (grid, _temp_dir) = grid_with_corrupted_scrollback();
        let result = grid.try_get_history_line(0);
        assert!(
            result.is_err(),
            "corrupted cold-tier line should return Err, got: {result:?}"
        );
    }

    /// Regression (#4496): the compatibility wrapper `get_history_line` returns
    /// `None` (not panic) for corrupted scrollback, preserving the `Option` API.
    #[test]
    fn get_history_line_returns_none_on_error() {
        let (grid, _temp_dir) = grid_with_corrupted_scrollback();
        let result = grid.get_history_line(0);
        assert!(
            result.is_none(),
            "compatibility wrapper should return None for corrupted line"
        );
    }

    /// Regression (#4496): `try_history_line_rev` propagates errors for the
    /// reverse-index access path used by scrollback rendering.
    #[test]
    fn try_history_line_rev_propagates_scrollback_error() {
        let (grid, _temp_dir) = grid_with_corrupted_scrollback();
        // rev_idx 0 = most recent scrollback line, which is in the corrupted tier.
        // The newest lines might still be in hot/warm (both 0-limit), so use a
        // high rev_idx to hit the cold tier.
        let total = grid.scrollback_lines();
        let result = grid.try_history_line_rev(total.saturating_sub(1));
        assert!(
            result.is_err(),
            "reverse-index access to corrupted cold line should return Err, got: {result:?}"
        );
    }

    /// Regression (#4496): `SearchContent::get_row_text` for Grid does not panic
    /// on corrupted scrollback and returns `None` instead.
    #[test]
    fn search_content_returns_none_on_corrupted_scrollback() {
        use aterm_types::SearchContent;

        let (mut grid, _temp_dir) = grid_with_corrupted_scrollback();
        let result = grid.get_row_text(0);
        assert!(
            result.is_none(),
            "SearchContent should return None for corrupted scrollback, not panic"
        );
    }
}
