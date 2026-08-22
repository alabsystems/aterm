// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid-local facade for pin and generation tracking helpers extracted to
//! `aterm-grid`.
//!
//! Normal runtime builds only need `GenerationTracker`. The concrete `Pin` and
//! `PinnedRange` helpers remain test/Kani-only at this layer until a production
//! caller needs them across the crate boundary.

#[cfg(kani)]
use crate::Generation;
#[cfg(kani)]
use crate::GenerationTracker;
#[cfg(kani)]
use crate::{Pin, PinnedRange};

// ============================================================================
// Grid impl for pin operations
// ============================================================================

use super::Grid;

impl Grid {
    /// Get the absolute row counter (monotonically increasing).
    #[must_use]
    #[inline]
    pub fn absolute_row_counter(&self) -> u64 {
        self.storage.absolute_row_counter
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

    /// The absolute row number of the OLDEST line still retained.
    ///
    /// History index 0 (the oldest retained scrollback line) corresponds to this
    /// absolute row. Lines with an absolute row `< oldest_absolute_row()` have
    /// been EVICTED from scrollback and can no longer be read.
    ///
    /// Formula: `absolute_row_counter - visible_rows - scrollback_lines()`.
    /// The top visible row is at absolute `absolute_row_counter - visible_rows`;
    /// the oldest scrollback line sits `scrollback_lines()` rows above it.
    #[must_use]
    pub fn oldest_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.scrollback_lines() as u64)
    }

    /// Monotonic epoch of history-row RENUMBERINGS that are invisible to the
    /// `(content_gen, absolute_row_revision)` pair — see
    /// `GridStorage::history_renumber_epoch`. An advance means the absolute
    /// keys of retained history rows may have shifted wholesale (Kitty CSI +T
    /// unscroll removed the newest scrollback lines): absolute-row-keyed
    /// incremental caches must REBUILD rather than refresh.
    #[must_use]
    #[inline]
    pub fn history_renumber_epoch(&self) -> u64 {
        self.storage.history_renumber_epoch
    }

    /// The absolute row number currently shown at the TOP of the viewport,
    /// accounting for the scrollback `display_offset`. At the live bottom this is
    /// `absolute_row_counter - visible_rows`; scrolled up by `display_offset` it
    /// is that many rows earlier. The reference point for prompt-to-prompt
    /// navigation (which mark is "above" vs "below" the current view).
    #[must_use]
    pub fn top_visible_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.storage.display_offset as u64)
    }

    /// Absolute row index of the live/last line (xterm `buffer.active.baseY`).
    ///
    /// Equals `oldest_absolute_row() + scrollback_lines()` — the absolute row of
    /// the top visible line when NOT scrolled into history. Returned as `usize`
    /// so the wasm bridge maps it to a plain JS number (not BigInt).
    #[must_use]
    #[inline]
    pub fn base_y(&self) -> usize {
        self.oldest_absolute_row() as usize + self.scrollback_lines()
    }

    /// Absolute row index of the TOP visible line for the current viewport.
    ///
    /// `base_y()` minus the scroll-back `display_offset`; equals `base_y()` when
    /// not scrolled. Used as the search/link origin so a hit's absolute row can
    /// be mapped back to a display row.
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

    // TODO(#7932): tautology — strengthen or delete — T1: constructor round-trip field == any-binding
    #[kani::proof]
    fn pin_absolute_row_roundtrip() {
        let row: u64 = kani::any();
        let col: u16 = kani::any();
        let generation: Generation = kani::any();

        let pin = Pin::from_absolute(row, col, generation);
        kani::assert(pin.absolute_row() == row, "absolute row should roundtrip");
        kani::assert(pin.col() == col, "col should be preserved");
        kani::assert(
            pin.generation() == generation,
            "generation should be preserved",
        );
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
