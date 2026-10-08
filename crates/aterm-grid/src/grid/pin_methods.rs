// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid-local facade for the pin and generation tracking helpers. Runtime
//! builds use only `GenerationTracker`; `Pin` and `PinnedRange` are
//! test/Kani-only here.

#[cfg(kani)]
use crate::Generation;
#[cfg(kani)]
use crate::GenerationTracker;
#[cfg(kani)]
use crate::{Pin, PinnedRange};

// Grid impl for pin operations

use super::Grid;

impl Grid {
    /// Get the absolute row counter (monotonically increasing).
    #[must_use]
    #[inline]
    pub fn absolute_row_counter(&self) -> u64 {
        self.storage.absolute_row_counter
    }

    /// Continue another grid's absolute row numbering after a checkpoint
    /// restore, so an absolute row recorded before the handoff (a shell mark,
    /// a pin) names the same line after it. Only for a grid nothing has
    /// observed yet.
    ///
    /// Clamped: never below what this grid already holds, and never within
    /// `2^16` rows of `u64::MAX`, so a forged counter cannot overflow a scroll.
    pub fn continue_absolute_numbering(&mut self, counter: u64) {
        const CEILING: u64 = u64::MAX >> 16;
        let floor =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        self.storage.absolute_row_counter = counter.min(CEILING).max(floor);
    }

    /// EVERY RETAINED ROW KEEPS A KEY AT OR ABOVE 0: raise the counter to the
    /// retained total (`visible_rows + scrollback_lines()`) where a resize left
    /// it below — the floor [`Self::continue_absolute_numbering`] keeps for a
    /// restore, kept for every resize.
    ///
    /// The two maps of an absolute row agree only above that floor: a mark is
    /// RECORDED through [`Self::visible_to_absolute`] (`counter − visible + r`)
    /// and read back through [`Self::oldest_absolute_row`] (`counter − visible −
    /// scrollback`, saturating at 0) plus the history count. Below it the read
    /// map saturates and the write map does not, so every OSC 133 mark a shell
    /// records from then on reads back as a row above its prompt (`line`,
    /// `blocks`; tab #1, 2026-10-01), for good — output moves the counter and
    /// the history together. A rows-only resize no longer gets here (its bottom
    /// arms move the counter with their rows); a WIDTH rewrap can, since it
    /// changes how many rows the same lines take (synchronously, or at the
    /// re-attach of an offloaded one). Raising the counter moves the write map
    /// onto the read map — the keys every read already used — and is reported
    /// as the renumbering it is to the write side.
    pub(crate) fn cover_retained_rows(&mut self) {
        let retained =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        if self.storage.absolute_row_counter < retained {
            self.storage.absolute_row_counter = retained;
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
            self.invalidate_host_coordinates();
        }
    }

    /// Convert visible coordinates to absolute row number.
    /// Formula: `absolute_row_counter - visible_rows + visible_row`.
    #[must_use]
    pub fn visible_to_absolute(&self, visible_row: u16) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_add(u64::from(visible_row))
    }

    /// The absolute row number of the oldest retained line (history index 0).
    /// Rows below it have been evicted.
    ///
    /// Formula: `absolute_row_counter - visible_rows - scrollback_lines()`.
    #[must_use]
    pub fn oldest_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.scrollback_lines() as u64)
    }

    /// Epoch of row renumberings invisible to `(content_gen,
    /// absolute_row_revision)` (see `GridStorage::history_renumber_epoch`): a
    /// width reflow or a Kitty CSI +T unscroll. On an advance, absolute-row-keyed
    /// caches rebuild and cached absolute rows (search hits, find anchors) are
    /// recomputed before use.
    #[must_use]
    #[inline]
    pub fn history_renumber_epoch(&self) -> u64 {
        self.storage.history_renumber_epoch
    }

    /// The absolute row shown at the top of the viewport:
    /// `absolute_row_counter - visible_rows - display_offset`. The reference
    /// point for prompt-to-prompt navigation.
    #[must_use]
    pub fn top_visible_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.storage.display_offset as u64)
    }

    /// Absolute row of the top visible line at the live bottom (xterm
    /// `buffer.active.baseY`): `oldest_absolute_row() + scrollback_lines()`.
    /// `usize` so wasm maps it to a plain JS number.
    #[must_use]
    #[inline]
    pub fn base_y(&self) -> usize {
        self.oldest_absolute_row() as usize + self.scrollback_lines()
    }

    /// Absolute row of the top visible line for the current viewport:
    /// `base_y() - display_offset`. The search/link origin.
    #[must_use]
    #[inline]
    pub fn display_origin_absolute(&self) -> usize {
        self.base_y().saturating_sub(self.display_offset())
    }
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;

#[cfg(kani)]
mod proofs {
    use super::*;

    /// The storage-format contract: an absolute row splits into a page id (the
    /// high 32 bits) and a row offset (the low 32) and recombines losslessly.
    /// `row >> 32` always fits a 32-bit `usize`, so this holds on wasm32 too.
    #[kani::proof]
    fn pin_absolute_row_packing_is_lossless() {
        let row: u64 = kani::any();
        let pin = Pin::from_absolute(row, kani::any(), kani::any());
        kani::assert(
            pin.page_id() == (row >> 32) as usize,
            "page id is the high half",
        );
        kani::assert(pin.row_offset() == row as u32, "row offset is the low half");
        kani::assert(pin.absolute_row() == row, "the split recombines losslessly");
    }

    #[kani::proof]
    fn generation_tracker_evict_increments() {
        let page_id: usize = kani::any();
        kani::assume(page_id < 16); // Limit for tractability

        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(page_id + 1);

        let gen_before = tracker.page_generation(page_id);
        let global_before = tracker.current_generation();

        tracker.evict_page(page_id);

        kani::assert(
            tracker.page_generation(page_id) == gen_before + 1,
            "page generation should increment",
        );
        kani::assert(
            tracker.current_generation() == global_before + 1,
            "global generation should increment",
        );
    }

    #[kani::proof]
    fn pin_invalidated_after_eviction() {
        let page_id: usize = kani::any();
        kani::assume(page_id < 8);

        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(page_id + 1);

        // Create pin at current generation
        let generation = tracker.page_generation(page_id);
        let pin = Pin::new(page_id, 0, 0, generation);

        kani::assert(tracker.is_valid(&pin), "pin should be valid initially");

        // Evict the page
        tracker.evict_page(page_id);

        kani::assert(
            !tracker.is_valid(&pin),
            "pin should be invalid after eviction",
        );
    }

    #[kani::proof]
    fn pinned_range_normalization_preserves_content() {
        let row1: u32 = kani::any();
        let row2: u32 = kani::any();
        let col1: u16 = kani::any();
        let col2: u16 = kani::any();

        let start = Pin::from_absolute(row1 as u64, col1, 0);
        let end = Pin::from_absolute(row2 as u64, col2, 0);
        let range = PinnedRange::new(start, end);
        let normalized = range.normalized();

        // Normalized range should have start <= end
        let start_before = normalized.start.absolute_row() < normalized.end.absolute_row()
            || (normalized.start.absolute_row() == normalized.end.absolute_row()
                && normalized.start.col() <= normalized.end.col());

        kani::assert(start_before, "normalized range should have start <= end");
    }

    #[kani::proof]
    fn evict_pages_from_invalidates_range() {
        let first_page: usize = kani::any();
        kani::assume(first_page < 8);

        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(first_page + 4);

        // Create a pin on a page that will be evicted
        let target_page = first_page + 1;
        let pin = Pin::new(target_page, 0, 0, tracker.page_generation(target_page));

        kani::assert(
            tracker.is_valid(&pin),
            "pin should be valid before eviction",
        );

        tracker.evict_pages_from(first_page);

        kani::assert(
            !tracker.is_valid(&pin),
            "pin should be invalid after evict_from",
        );
    }
}
