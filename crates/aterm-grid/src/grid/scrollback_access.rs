// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scrollback query and management for [`Grid`]: history line access over
//! the tiered store, the lazy buffer and the ring.

use std::borrow::Cow;

use aterm_scrollback::ScrollbackError;
use aterm_scrollback::{Line, ScrollbackStorage};

use super::{Grid, scroll_convert, scroll_materialize};

impl Grid {
    /// Get a mutable reference to the scrollback storage, if attached.
    ///
    /// Drains the lazy buffer first so all deferred lines are available
    /// in the tiered scrollback.
    #[inline]
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

    /// Lines in the tiered store plus the lazy buffer.
    #[must_use]
    #[inline]
    pub fn tiered_scrollback_lines(&self) -> usize {
        self.storage.tiered_scrollback_lines()
    }

    /// A history line by index (0 = oldest): the tiered store (oldest), then the
    /// lazy buffer, then the ring (newest). `Ok(None)` when out of bounds.
    ///
    /// `&self` suffices: the tiers' decode caches and the deferred line's
    /// `OnceCell` are interior-mutable.
    pub fn try_get_history_line(
        &self,
        idx: usize,
    ) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
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
            // Tiered store: hot lines borrow, warm/cold lines decode owned.
            let Some(scrollback) = self.storage.scrollback() else {
                return Ok(None);
            };
            scrollback.get_line(idx)
        } else if idx < actual_tiered + lazy_count {
            // Lazy buffer: a deferred line materializes on first access.
            let lazy_idx = idx - actual_tiered;
            match self.storage.lazy_buffer.get_line(lazy_idx) {
                Some(line) => Ok(Some(Cow::Borrowed(line))),
                None => Ok(None),
            }
        } else {
            // Ring rows are always converted to an owned line.
            let ring_idx = idx - actual_tiered - lazy_count;
            let Some(row) = self.storage.ring_history_row(ring_idx) else {
                return Ok(None);
            };
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
    /// [`get_history_line(start + k)`](Self::get_history_line) returns, `None`
    /// for an unreadable line.
    ///
    /// The tiered store is walked by [`ScrollbackStorage::dense_from`] (one
    /// decode per warm block or cold page) instead of a per-line search, cache
    /// probe and clone. An undecodable segment yields one placeholder per line,
    /// so item `k` always keys row `oldest + start + k`.
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
    #[inline]
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
    #[inline]
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
    #[inline]
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
    /// Rebuilds the survivors into a fresh page store (as `erase_scrollback`
    /// does) to reclaim the evicted pages. Survivors keep their absolute-row
    /// identity: only `total_lines` shrinks, so `oldest_absolute_row()` advances
    /// by `n`, as on `scroll_up`'s at-capacity eviction.
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
        // The retained line set changed: re-clamp a scrolled-back viewport and
        // invalidate content-keyed caches (search index, cross-session polls).
        self.clamp_display_offset();
        self.storage.mark_content_full();
    }

    /// The ring row behind reverse history index `rev_idx`, with its extras, or
    /// `None` when the index is in the lazy buffer or tiered store. The ring
    /// holds the newest `ring_buffer_scrollback()` lines, so the ring count alone
    /// answers it, with no `dyn` `line_count()` on the store per declined row
    /// (this runs per visible row on every history read).
    fn ring_history_row_at_rev(
        &self,
        rev_idx: usize,
    ) -> Option<(&super::Row, Option<&scroll_convert::ScrolledRowExtras>)> {
        let ring = self.storage.ring_buffer_scrollback();
        // A `rev_idx` past the ring underflows here: that is the decline, and
        // the caller reads it as a `Line`.
        let ring_idx = ring.checked_sub(rev_idx.checked_add(1)?)?;
        let row = self.storage.ring_history_row(ring_idx)?;
        Some((row, self.storage.ring_history_extras(ring_idx)))
    }

    /// Materialize scrollback line `rev_idx` (0 = newest) with full fidelity:
    /// cells plus their extras (hyperlinks, complex chars, RGB colors), read
    /// through `MaterializedRow::get_extra` as for visible cells.
    ///
    /// Ring tier first: the newest lines' rows are still in RAM as `Cell`s, and
    /// [`materialize_from_row_extras`](scroll_materialize::materialize_from_row_extras)
    /// reads them directly instead of round-tripping through text + RLE. It
    /// declines whenever it cannot prove the same result; `history_line_rev`
    /// answers the declined cases and lines beyond the ring.
    ///
    /// An early-wrap hole closing the row is painted as the print path painted
    /// it, from the wide glyph that opens the row after
    /// ([`MaterializedRow::paint_hole`](scroll_materialize::MaterializedRow::paint_hole)):
    /// the line left the hole out, as it is no text.
    #[must_use]
    pub fn materialize_scrollback_row_full(
        &self,
        rev_idx: usize,
        cols: u16,
    ) -> Option<scroll_materialize::MaterializedRow> {
        let mut mat = self.materialize_history_row(rev_idx, cols)?;
        if mat.hole
            && let Some((glyph, bg_rgb)) = self.history_wrap_opening(rev_idx, cols)
        {
            mat.paint_hole(glyph, bg_rgb);
        }
        Some(mat)
    }

    /// History row `rev_idx` as its line draws it, with no hole painted.
    fn materialize_history_row(
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

    /// The wide glyph that opens the row after history row `rev_idx`, with its
    /// truecolour background, when that row continues it: the glyph whose
    /// early wrap left a hole at the end of `rev_idx`. The newest line's
    /// continuation is the screen's top row.
    fn history_wrap_opening(
        &self,
        rev_idx: usize,
        cols: u16,
    ) -> Option<(crate::Cell, Option<[u8; 3]>)> {
        let Some(newer) = rev_idx.checked_sub(1) else {
            let glyph = super::scroll_fill::wrap_opening(self.storage.row_at_screen(0)?)?;
            return Some((glyph, self.storage.extras.bg_rgb_for(0, 0)));
        };
        if let Some((row, extras)) = self.ring_history_row_at_rev(newer) {
            let glyph = super::scroll_fill::wrap_opening(row)?;
            let bg_rgb = extras.and_then(|extras| {
                extras
                    .rgb_bg
                    .iter()
                    .find_map(|&(col, rgb)| (col == 0).then_some(rgb))
            });
            return Some((glyph, bg_rgb));
        }
        let line = self.history_line_rev(newer)?;
        let next = scroll_materialize::materialize_from_line(&line, cols);
        let glyph = *next.cells.first()?;
        (next.is_wrapped() && glyph.is_wide())
            .then(|| (glyph, next.get_extra(0).and_then(crate::CellExtra::bg_rgb)))
    }

    /// Total history lines; same as [`Self::scrollback_lines`].
    #[must_use]
    #[inline]
    pub fn history_line_count(&self) -> usize {
        self.storage.scrollback_lines()
    }
}

/// Dense forward walk over a grid's history ([`Grid::history_lines_from`]):
/// one item per line from the start index to the newest, in order: tiered
/// store, lazy buffer, ring.
pub struct HistoryLines<'a> {
    grid: &'a Grid,
    /// The tiered store's dense walk, while the cursor is still inside it.
    tiered: Option<aterm_scrollback::DenseScrollbackIter<'a>>,
    /// History index of the next item.
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

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end - self.idx;
        (n, Some(n))
    }
}

impl ExactSizeIterator for HistoryLines<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_scrollback::Scrollback;

    /// A grid whose tiered history starts with a corrupt warm block (three
    /// lines that fail to decompress) ahead of one readable hot line.
    fn grid_with_corrupted_scrollback() -> Grid {
        let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
        sb.push_str("Line00");
        sb.inject_corrupted_warm_block(3);
        let mut grid = Grid::new(5, 80);
        grid.attach_scrollback(sb);
        grid
    }

    /// A corrupted tier surfaces as an error on the fallible reads (by index
    /// and by reverse index) and as `None`, never a panic, on the
    /// compatibility wrapper.
    #[test]
    fn corrupted_scrollback_reads_fail_without_panicking() {
        let grid = grid_with_corrupted_scrollback();
        let result = grid.try_get_history_line(0);
        assert!(result.is_err(), "try_get_history_line: {result:?}");
        assert!(grid.get_history_line(0).is_none());
        let total = grid.scrollback_lines();
        let result = grid.try_history_line_rev(total.saturating_sub(1));
        assert!(result.is_err(), "try_history_line_rev: {result:?}");
    }
}
