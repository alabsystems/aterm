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

    /// Total scrollback lines (tiered + lazy-staged + ring).
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
            // Use preserved extras from ring_extras.
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

    /// Dense forward walk over history from index `start` (0 = oldest) to the
    /// newest line: item `k` is exactly what
    /// [`get_history_line(start + k)`](Self::get_history_line) returns, with
    /// `None` for an unreadable line.
    ///
    /// The tiered store is walked via [`ScrollbackStorage::dense_from`] (one
    /// decode per warm block / cold page, lines moved out, hot lines borrowed,
    /// one warning per corrupt segment) instead of a per-line search, cache
    /// probe and clone. The lazy buffer and ring are read as the per-line
    /// reader reads them. An undecodable segment yields one placeholder per
    /// line, so `oldest + start + k` always keys the right row. Checked against
    /// the per-line reader by `history_lines_from_matches_get_history_line_*`.
    #[must_use]
    pub fn history_lines_from(&self, start: usize) -> HistoryLines<'_> {
        // The same three counts `try_get_history_line` partitions by.
        let tiered = self.storage.scrollback.as_ref();
        let tiered_end = tiered.map_or(0, ScrollbackStorage::line_count);
        let lazy_end = tiered_end.saturating_add(self.storage.lazy_buffer_lines());
        let end = lazy_end.saturating_add(self.storage.ring_buffer_scrollback());
        let idx = start.min(end);
        HistoryLines {
            grid: self,
            tiered: tiered
                .filter(|_| idx < tiered_end)
                .map(|sb| sb.dense_from(idx)),
            idx,
            tiered_end,
            lazy_end,
            end,
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
    /// Rebuilds the retained rows into a fresh page store (as
    /// `erase_scrollback` does) so the evicted lines' page memory is
    /// reclaimed. Survivors keep their absolute-row identity: only
    /// `total_lines` shrinks, so `oldest_absolute_row()` advances by `n`, like
    /// `scroll_up`'s at-capacity eviction.
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

    /// The ring-tier row behind reverse history index `rev_idx`, with its
    /// preserved extras, or `None` when that index is in the lazy buffer or
    /// tiered store.
    ///
    /// The ring holds exactly the newest `ring_buffer_scrollback()` lines
    /// (`scrollback_lines() == tiered + lazy + ring`), and `rev_idx` counts
    /// back from the newest, so the ring count alone answers it: ring line iff
    /// `rev_idx < ring`, at `ring - 1 - rev_idx`. This avoids a `dyn`
    /// `line_count()` call on the tiered store for every declined row, since
    /// it runs per visible row on every history read.
    fn ring_history_row_at_rev(
        &self,
        rev_idx: usize,
    ) -> Option<(&super::Row, Option<&scroll_convert::ScrolledRowExtras>)> {
        let ring = self.storage.ring_buffer_scrollback();
        // `checked_*`: a `rev_idx` past the ring underflows here, and that is
        // the decline; the caller then reads it as a `Line`.
        let ring_idx = ring.checked_sub(rev_idx.checked_add(1)?)?;
        let row = self.storage.ring_history_row(ring_idx)?;
        Some((row, self.storage.ring_history_extras(ring_idx)))
    }

    /// Materialize a scrollback line with full fidelity.
    ///
    /// Returns a [`MaterializedRow`](scroll_materialize::MaterializedRow)
    /// containing cells plus supplementary extras (hyperlinks, complex chars,
    /// RGB colors). The bridge renderer queries
    /// `MaterializedRow::get_extra(col)` for scrollback cells using the same
    /// code path as visible-area cells.
    ///
    /// Ring tier first: for the newest lines the source row is still in RAM as
    /// `Cell`s, and
    /// [`materialize_from_row_extras`](scroll_materialize::materialize_from_row_extras)
    /// reads it directly instead of serializing to text + RLE and parsing
    /// back. It returns `None` whenever it cannot prove the same result, so the
    /// `history_line_rev` path (whose hot-tier `Cow` avoids a clone) remains
    /// the answer for declined cases and lines beyond the ring.
    #[must_use]
    pub fn materialize_scrollback_row_full(
        &self,
        rev_idx: usize,
        cols: u16,
    ) -> Option<scroll_materialize::MaterializedRow> {
        if let Some((row, extras)) = self.ring_history_row_at_rev(rev_idx)
            && let Some(mat) = scroll_materialize::materialize_from_row_extras(row, extras, cols)
        {
            return Some(mat);
        }
        let line = self.history_line_rev(rev_idx)?;
        Some(scroll_materialize::materialize_from_line(&line, cols))
    }

    /// The cells history line `idx` (0 = oldest) is drawn in, when the ring
    /// tier draws it from its own cells: `push(bytes, col, width, count)` in
    /// column order, as [`stored_line_cells`](scroll_materialize::stored_line_cells)
    /// yields them, `bytes` the first cell's text in `text`, the line's text
    /// as read ([`history_line_rev`](Self::history_line_rev) or the dense
    /// walk, bounded), and `true`.
    ///
    /// `false` when the row is drawn by
    /// [`stored_line_cells`](scroll_materialize::stored_line_cells), the walk
    /// to read it with: a line past the ring tier, or a ring row the fast
    /// path declines. The cells pushed before a `false` are not the row's.
    ///
    /// Why (r16 selection-stride review, 2026-10-11): the ring tier draws a
    /// row from its cells ([`materialize_scrollback_row_full`](Self::materialize_scrollback_row_full)),
    /// which keep apart what the stored walk joins (two flag halves drawn out
    /// of order stay two cells), so reading every history row's cells from
    /// its stored line put a double-click's word, a copy's columns and a
    /// find match up to two cells off where the ring tier draws them.
    pub fn ring_history_cells(
        &self,
        idx: usize,
        text: &str,
        push: impl FnMut(std::ops::Range<usize>, u16, u16, u16),
    ) -> bool {
        let Some(rev_idx) = idx
            .checked_add(1)
            .and_then(|n| self.storage.scrollback_lines().checked_sub(n))
        else {
            return false;
        };
        let Some((row, extras)) = self.ring_history_row_at_rev(rev_idx) else {
            return false;
        };
        scroll_materialize::ring_row_cells(row, extras, self.cols(), text, push)
    }

    /// Total history lines; same as [`Self::scrollback_lines`].
    #[must_use]
    #[inline]
    pub fn history_line_count(&self) -> usize {
        self.storage.scrollback_lines()
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
        // Windows: the cold tier maps the file and Windows refuses to shrink a
        // mapped file (ERROR_USER_MAPPED_FILE), so corrupt it by overwriting
        // every page byte past the 32-byte header instead (every cold-page read
        // fails either way).
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

    /// `try_get_history_line` propagates scrollback errors instead of masking
    /// them as `None`.
    #[test]
    fn try_get_history_line_propagates_scrollback_error() {
        let (grid, _temp_dir) = grid_with_corrupted_scrollback();
        let result = grid.try_get_history_line(0);
        assert!(
            result.is_err(),
            "corrupted cold-tier line should return Err, got: {result:?}"
        );
    }

    /// The compatibility wrapper `get_history_line` returns `None` (not panic)
    /// for corrupted scrollback.
    #[test]
    fn get_history_line_returns_none_on_error() {
        let (grid, _temp_dir) = grid_with_corrupted_scrollback();
        let result = grid.get_history_line(0);
        assert!(
            result.is_none(),
            "compatibility wrapper should return None for corrupted line"
        );
    }

    /// `try_history_line_rev` propagates errors on the reverse-index path used
    /// by scrollback rendering.
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

    /// `SearchContent::get_row_text` for Grid returns `None` rather than
    /// panicking on corrupted scrollback.
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

/// Dense forward walk over a grid's history — see
/// [`Grid::history_lines_from`]. Exactly one item per history line from the
/// start index to the newest, in order: tiered store, then the lazy buffer,
/// then the ring.
pub struct HistoryLines<'a> {
    grid: &'a Grid,
    /// The tiered store's dense walk, while the cursor is still inside it.
    tiered: Option<aterm_scrollback::DenseScrollbackIter<'a>>,
    /// History index of the NEXT item.
    idx: usize,
    tiered_end: usize,
    lazy_end: usize,
    end: usize,
}

impl<'a> Iterator for HistoryLines<'a> {
    type Item = Option<Cow<'a, Line>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.end {
            return None;
        }
        let idx = self.idx;
        self.idx += 1;
        if idx < self.tiered_end {
            // The dense walk yields exactly `tiered_end - start` items, so it
            // cannot run dry inside the tiered range; `flatten` keeps a
            // defective store a placeholder rather than a panic.
            return Some(self.tiered.as_mut().and_then(Iterator::next).flatten());
        }
        self.tiered = None;
        let storage = &self.grid.storage;
        if idx < self.lazy_end {
            return Some(
                storage
                    .lazy_buffer
                    .get_line(idx - self.tiered_end)
                    .map(Cow::Borrowed),
            );
        }
        let ring_idx = idx - self.lazy_end;
        let Some(row) = storage.ring_history_row(ring_idx) else {
            return Some(None);
        };
        let default_extras = scroll_convert::ScrolledRowExtras::default();
        let extras = storage
            .ring_history_extras(ring_idx)
            .unwrap_or(&default_extras);
        Some(Some(Cow::Owned(Grid::row_to_line_with_stored_extras(
            row, extras,
        ))))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end - self.idx;
        (n, Some(n))
    }
}

impl ExactSizeIterator for HistoryLines<'_> {}
