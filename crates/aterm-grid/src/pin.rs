// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pin and generation tracking helpers for stable grid references.

use super::page::PageId;

/// Generation counter for detecting stale pins.
pub type Generation = u64;

/// A stable reference to a position in the terminal buffer.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    /// Page containing the pinned content.
    page_id: PageId,
    /// Row offset within the page (0-indexed from page start).
    row_offset: u32,
    /// Column position (0-indexed).
    col: u16,
    /// Generation at pin creation time.
    generation: Generation,
}

#[cfg(test)]
impl Pin {
    /// Create a new pin at the given position.
    #[must_use]
    pub(crate) const fn new(
        page_id: PageId,
        row_offset: u32,
        col: u16,
        generation: Generation,
    ) -> Self {
        Self {
            page_id,
            row_offset,
            col,
            generation,
        }
    }

    /// Create a pin from absolute coordinates.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "intentional truncation for storage format"
    )]
    pub(crate) const fn from_absolute(absolute_row: u64, col: u16, generation: Generation) -> Self {
        Self {
            page_id: (absolute_row >> 32) as usize,
            row_offset: absolute_row as u32,
            col,
            generation,
        }
    }

    /// Get the page ID.
    #[must_use]
    pub(crate) const fn page_id(&self) -> PageId {
        self.page_id
    }

    /// Get the row offset within the page.
    #[must_use]
    pub(crate) const fn row_offset(&self) -> u32 {
        self.row_offset
    }

    /// Get the column position.
    #[must_use]
    pub(crate) const fn col(&self) -> u16 {
        self.col
    }

    /// Get the generation at pin creation.
    #[must_use]
    pub(crate) const fn generation(&self) -> Generation {
        self.generation
    }

    /// Get the absolute row number (for from_absolute pins).
    #[must_use]
    pub(crate) const fn absolute_row(&self) -> u64 {
        ((self.page_id as u64) << 32) | (self.row_offset as u64)
    }

    /// Create a new pin with updated column.
    #[must_use]
    pub(crate) const fn with_col(self, col: u16) -> Self {
        Self { col, ..self }
    }

    /// Create a new pin with updated row offset.
    #[must_use]
    pub(crate) const fn with_row_offset(self, row_offset: u32) -> Self {
        Self { row_offset, ..self }
    }
}

/// Tracks generations for pages to detect pin invalidation.
#[derive(Debug, Clone)]
pub struct GenerationTracker {
    /// Current generation for each page.
    generations: Vec<Generation>,
    /// Global generation counter (increments on any page eviction).
    global_generation: Generation,
    /// Minimum valid generation (pins older than this are definitely invalid).
    min_valid_generation: Generation,
}

impl Default for GenerationTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl GenerationTracker {
    /// Create a new generation tracker.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            generations: Vec::new(),
            global_generation: 0,
            min_valid_generation: 0,
        }
    }

    /// Get the current global generation.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn current_generation(&self) -> Generation {
        self.global_generation
    }

    /// Get the generation for a specific page.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn page_generation(&self, page_id: PageId) -> Generation {
        self.generations.get(page_id).copied().unwrap_or(0)
    }

    /// Ensure we have generation tracking for the given page count.
    pub(crate) fn ensure_capacity(&mut self, page_count: usize) {
        if self.generations.len() < page_count {
            self.generations.resize(page_count, 0);
        }
    }

    /// Mark a page as evicted (increment its generation).
    pub(crate) fn evict_page(&mut self, page_id: PageId) {
        self.ensure_capacity(page_id + 1);
        self.generations[page_id] += 1;
        self.global_generation += 1;
    }

    /// Mark multiple pages as evicted starting from `first_page`.
    #[cfg(test)]
    pub(crate) fn evict_pages_from(&mut self, first_page: PageId) {
        for page_id in first_page..self.generations.len() {
            self.generations[page_id] += 1;
        }
        self.global_generation += 1;
    }

    /// Invalidate all pins by updating the minimum valid generation.
    pub(crate) fn evict_all(&mut self) {
        for generation in &mut self.generations {
            *generation += 1;
        }
        self.global_generation += 1;
        self.min_valid_generation = self.global_generation;
    }

    /// Check if a pin is potentially valid.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_potentially_valid(&self, pin: &Pin) -> bool {
        if pin.generation < self.min_valid_generation {
            return false;
        }

        let page_generation = self.page_generation(pin.page_id);
        pin.generation >= page_generation
    }

    /// Check if a pin matches the current generation of its page.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_valid(&self, pin: &Pin) -> bool {
        if pin.generation < self.min_valid_generation {
            return false;
        }

        let page_generation = self.page_generation(pin.page_id);
        pin.generation == page_generation
    }
}

/// A pinned range (start and end pins).
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedRange {
    /// Start of the range (inclusive).
    pub start: Pin,
    /// End of the range (inclusive).
    pub end: Pin,
}

#[cfg(test)]
impl PinnedRange {
    /// Create a new pinned range.
    #[must_use]
    pub(crate) const fn new(start: Pin, end: Pin) -> Self {
        Self { start, end }
    }

    /// Check if both pins are valid.
    #[must_use]
    pub(crate) fn is_valid(&self, tracker: &GenerationTracker) -> bool {
        tracker.is_valid(&self.start) && tracker.is_valid(&self.end)
    }

    /// Create a normalized range (start <= end).
    #[must_use]
    pub(crate) fn normalized(self) -> Self {
        if self.start.absolute_row() > self.end.absolute_row()
            || (self.start.absolute_row() == self.end.absolute_row()
                && self.start.col > self.end.col)
        {
            Self {
                start: self.end,
                end: self.start,
            }
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pin: construction and getters

    #[test]
    fn test_pin_new_stores_all_fields() {
        let pin = Pin::new(3, 42, 7, 100);
        assert_eq!(pin.page_id(), 3);
        assert_eq!(pin.row_offset(), 42);
        assert_eq!(pin.col(), 7);
        assert_eq!(pin.generation(), 100);
    }

    // Pin: from_absolute and absolute_row roundtrip

    #[test]
    fn test_pin_from_absolute_zero() {
        let pin = Pin::from_absolute(0, 0, 0);
        assert_eq!(pin.absolute_row(), 0);
        assert_eq!(pin.col(), 0);
        assert_eq!(pin.generation(), 0);
    }

    #[test]
    fn test_pin_from_absolute_small_row() {
        let pin = Pin::from_absolute(42, 5, 10);
        assert_eq!(pin.absolute_row(), 42);
        assert_eq!(pin.col(), 5);
        assert_eq!(pin.generation(), 10);
    }

    #[test]
    fn test_pin_from_absolute_large_row_roundtrip() {
        // Test with a value that spans both page_id and row_offset
        let absolute_row: u64 = (3_u64 << 32) | 500;
        let pin = Pin::from_absolute(absolute_row, 10, 99);
        assert_eq!(pin.absolute_row(), absolute_row);
        assert_eq!(pin.page_id(), 3);
        assert_eq!(pin.row_offset(), 500);
    }

    #[test]
    fn test_pin_from_absolute_max_u32_row() {
        // Row that fits entirely in the lower 32 bits
        let pin = Pin::from_absolute(u64::from(u32::MAX), 0, 0);
        assert_eq!(pin.absolute_row(), u64::from(u32::MAX));
        assert_eq!(pin.page_id(), 0);
        assert_eq!(pin.row_offset(), u32::MAX);
    }

    // Pin: with_col and with_row_offset (immutable update)

    #[test]
    fn test_pin_with_col_updates_column_only() {
        let pin = Pin::new(1, 2, 3, 4);
        let updated = pin.with_col(99);
        assert_eq!(updated.col(), 99);
        assert_eq!(updated.page_id(), 1);
        assert_eq!(updated.row_offset(), 2);
        assert_eq!(updated.generation(), 4);
    }

    #[test]
    fn test_pin_with_row_offset_updates_row_only() {
        let pin = Pin::new(1, 2, 3, 4);
        let updated = pin.with_row_offset(99);
        assert_eq!(updated.row_offset(), 99);
        assert_eq!(updated.page_id(), 1);
        assert_eq!(updated.col(), 3);
        assert_eq!(updated.generation(), 4);
    }

    // GenerationTracker: construction and defaults

    #[test]
    fn test_generation_tracker_new_starts_at_zero() {
        let tracker = GenerationTracker::new();
        assert_eq!(tracker.current_generation(), 0);
    }

    #[test]
    fn test_generation_tracker_page_generation_unknown_page_is_zero() {
        let tracker = GenerationTracker::new();
        assert_eq!(
            tracker.page_generation(999),
            0,
            "untracked page should report generation 0"
        );
    }

    // GenerationTracker: ensure_capacity

    #[test]
    fn test_generation_tracker_ensure_capacity_grows() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(5);
        // All 5 pages should be accessible at generation 0
        for page_id in 0..5 {
            assert_eq!(tracker.page_generation(page_id), 0);
        }
    }

    #[test]
    fn test_generation_tracker_ensure_capacity_idempotent() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(5);
        tracker.evict_page(2);
        tracker.ensure_capacity(5); // should not reset
        assert_eq!(
            tracker.page_generation(2),
            1,
            "second ensure_capacity should not reset existing generations"
        );
    }

    #[test]
    fn test_generation_tracker_ensure_capacity_grows_larger() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        tracker.evict_page(1);
        tracker.ensure_capacity(10);
        assert_eq!(
            tracker.page_generation(1),
            1,
            "existing page generation should be preserved when growing"
        );
        assert_eq!(
            tracker.page_generation(9),
            0,
            "new page should start at generation 0"
        );
    }

    // GenerationTracker: evict_page

    #[test]
    fn test_evict_page_increments_page_generation() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        assert_eq!(tracker.page_generation(1), 0);
        tracker.evict_page(1);
        assert_eq!(tracker.page_generation(1), 1);
    }

    #[test]
    fn test_evict_page_increments_global_generation() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        assert_eq!(tracker.current_generation(), 0);
        tracker.evict_page(0);
        assert_eq!(tracker.current_generation(), 1);
    }

    #[test]
    fn test_evict_page_does_not_affect_other_pages() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        tracker.evict_page(1);
        assert_eq!(tracker.page_generation(0), 0, "page 0 should be unaffected");
        assert_eq!(tracker.page_generation(2), 0, "page 2 should be unaffected");
    }

    #[test]
    fn test_evict_page_multiple_times() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(2);
        tracker.evict_page(0);
        tracker.evict_page(0);
        tracker.evict_page(0);
        assert_eq!(tracker.page_generation(0), 3);
        assert_eq!(tracker.current_generation(), 3);
    }

    #[test]
    fn test_evict_page_auto_grows_capacity() {
        let mut tracker = GenerationTracker::new();
        // Evicting page 5 without prior ensure_capacity should auto-grow
        tracker.evict_page(5);
        assert_eq!(tracker.page_generation(5), 1);
    }

    // GenerationTracker: evict_pages_from

    #[test]
    fn test_evict_pages_from_evicts_all_from_start() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(5);
        tracker.evict_pages_from(2);
        assert_eq!(tracker.page_generation(0), 0, "page 0 before start");
        assert_eq!(tracker.page_generation(1), 0, "page 1 before start");
        assert_eq!(tracker.page_generation(2), 1, "page 2 at start");
        assert_eq!(tracker.page_generation(3), 1, "page 3 after start");
        assert_eq!(tracker.page_generation(4), 1, "page 4 after start");
    }

    #[test]
    fn test_evict_pages_from_increments_global_once() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(5);
        tracker.evict_pages_from(0);
        assert_eq!(
            tracker.current_generation(),
            1,
            "global generation should increment once regardless of page count"
        );
    }

    #[test]
    fn test_evict_pages_from_beyond_capacity_is_noop() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        tracker.evict_pages_from(10); // beyond capacity
        // No pages evicted, but global generation still increments
        assert_eq!(tracker.current_generation(), 1);
        assert_eq!(tracker.page_generation(0), 0);
        assert_eq!(tracker.page_generation(1), 0);
        assert_eq!(tracker.page_generation(2), 0);
    }

    // GenerationTracker: evict_all

    #[test]
    fn test_evict_all_increments_all_pages() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(4);
        tracker.evict_all();
        for page_id in 0..4 {
            assert_eq!(tracker.page_generation(page_id), 1);
        }
    }

    #[test]
    fn test_evict_all_updates_min_valid_generation() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(2);

        // Create a pin at generation 0
        let pin = Pin::new(0, 0, 0, 0);
        assert!(
            tracker.is_valid(&pin),
            "pin should be valid before evict_all"
        );

        tracker.evict_all();
        assert!(
            !tracker.is_potentially_valid(&pin),
            "pin should not be potentially valid after evict_all"
        );
    }

    // GenerationTracker: is_valid and is_potentially_valid

    #[test]
    fn test_is_valid_fresh_pin() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        assert!(tracker.is_valid(&pin));
    }

    #[test]
    fn test_is_valid_after_eviction_returns_false() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        tracker.evict_page(1);
        assert!(!tracker.is_valid(&pin));
    }

    #[test]
    fn test_is_valid_after_evicting_different_page() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        tracker.evict_page(2); // evict a different page
        assert!(
            tracker.is_valid(&pin),
            "pin should still be valid after evicting a different page"
        );
    }

    #[test]
    fn test_is_valid_pin_with_future_generation() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        // Pin with generation ahead of page generation
        let pin = Pin::new(1, 0, 0, 999);
        assert!(
            !tracker.is_valid(&pin),
            "pin with future generation should not be strictly valid"
        );
    }

    #[test]
    fn test_is_potentially_valid_fresh_pin() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        assert!(tracker.is_potentially_valid(&pin));
    }

    #[test]
    fn test_is_potentially_valid_after_eviction_returns_false() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        tracker.evict_page(1);
        assert!(
            !tracker.is_potentially_valid(&pin),
            "pin should not be potentially valid after its page is evicted"
        );
    }

    #[test]
    fn test_is_potentially_valid_allows_future_generation() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        // Pin with generation ahead of page generation is potentially valid
        // (it was created at a newer generation than the page)
        let pin = Pin::new(1, 0, 0, 999);
        assert!(
            tracker.is_potentially_valid(&pin),
            "pin with future generation should be potentially valid (>= page gen)"
        );
    }

    #[test]
    fn test_is_potentially_valid_below_min_valid_returns_false() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let pin = Pin::new(1, 0, 0, 0);
        tracker.evict_all(); // sets min_valid_generation
        assert!(
            !tracker.is_potentially_valid(&pin),
            "pin below min_valid_generation should not be potentially valid"
        );
    }

    #[test]
    fn test_is_valid_recreated_pin_after_eviction() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let old_pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        tracker.evict_page(1);
        assert!(!tracker.is_valid(&old_pin));

        // Re-create pin at new generation
        let new_pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        assert!(
            tracker.is_valid(&new_pin),
            "pin recreated at current generation should be valid"
        );
    }

    // GenerationTracker: evict_all invalidation completeness

    #[test]
    fn test_evict_all_invalidates_all_pages() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(4);

        // Create pins on every page
        let pins: Vec<Pin> = (0..4)
            .map(|page_id| Pin::new(page_id, 0, 0, tracker.page_generation(page_id)))
            .collect();

        for pin in &pins {
            assert!(tracker.is_valid(pin));
        }

        tracker.evict_all();

        for (i, pin) in pins.iter().enumerate() {
            assert!(
                !tracker.is_valid(pin),
                "pin on page {i} should be invalid after evict_all"
            );
        }
    }

    #[test]
    fn test_evict_all_then_recreate_pins_are_valid() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        tracker.evict_all();

        // Pins created after evict_all at current generation should be valid
        let pin = Pin::new(0, 0, 0, tracker.page_generation(0));
        assert!(
            tracker.is_valid(&pin),
            "pin created after evict_all at current gen should be valid"
        );
    }

    // PinnedRange: is_valid

    #[test]
    fn test_pinned_range_is_valid_both_pins_valid() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let start = Pin::new(0, 0, 0, tracker.page_generation(0));
        let end = Pin::new(1, 5, 10, tracker.page_generation(1));
        let range = PinnedRange::new(start, end);
        assert!(range.is_valid(&tracker));
    }

    #[test]
    fn test_pinned_range_is_valid_start_invalid() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let start = Pin::new(0, 0, 0, tracker.page_generation(0));
        let end = Pin::new(1, 5, 10, tracker.page_generation(1));
        tracker.evict_page(0);
        let range = PinnedRange::new(start, end);
        assert!(
            !range.is_valid(&tracker),
            "range should be invalid if start pin is invalid"
        );
    }

    #[test]
    fn test_pinned_range_is_valid_end_invalid() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let start = Pin::new(0, 0, 0, tracker.page_generation(0));
        let end = Pin::new(1, 5, 10, tracker.page_generation(1));
        tracker.evict_page(1);
        let range = PinnedRange::new(start, end);
        assert!(
            !range.is_valid(&tracker),
            "range should be invalid if end pin is invalid"
        );
    }

    #[test]
    fn test_pinned_range_is_valid_both_invalid() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);
        let start = Pin::new(0, 0, 0, tracker.page_generation(0));
        let end = Pin::new(1, 5, 10, tracker.page_generation(1));
        tracker.evict_all();
        let range = PinnedRange::new(start, end);
        assert!(!range.is_valid(&tracker));
    }

    // PinnedRange: normalized

    #[test]
    fn test_pinned_range_normalized_already_ordered() {
        let start = Pin::from_absolute(10, 0, 0);
        let end = Pin::from_absolute(20, 5, 0);
        let range = PinnedRange::new(start, end);
        let normalized = range.normalized();
        assert_eq!(normalized.start, start);
        assert_eq!(normalized.end, end);
    }

    #[test]
    fn test_pinned_range_normalized_swaps_reversed() {
        let start = Pin::from_absolute(20, 5, 0);
        let end = Pin::from_absolute(10, 0, 0);
        let range = PinnedRange::new(start, end);
        let normalized = range.normalized();
        assert_eq!(
            normalized.start.absolute_row(),
            10,
            "normalized start should be the smaller row"
        );
        assert_eq!(
            normalized.end.absolute_row(),
            20,
            "normalized end should be the larger row"
        );
    }

    #[test]
    fn test_pinned_range_normalized_same_row_different_col() {
        let start = Pin::from_absolute(10, 20, 0);
        let end = Pin::from_absolute(10, 5, 0);
        let range = PinnedRange::new(start, end);
        let normalized = range.normalized();
        assert_eq!(normalized.start.col(), 5, "smaller col should be start");
        assert_eq!(normalized.end.col(), 20, "larger col should be end");
    }

    #[test]
    fn test_pinned_range_normalized_same_row_same_col() {
        let pin = Pin::from_absolute(10, 5, 0);
        let range = PinnedRange::new(pin, pin);
        let normalized = range.normalized();
        assert_eq!(normalized.start, pin);
        assert_eq!(normalized.end, pin);
    }

    #[test]
    fn test_pinned_range_normalized_same_row_ordered_col() {
        let start = Pin::from_absolute(10, 5, 0);
        let end = Pin::from_absolute(10, 20, 0);
        let range = PinnedRange::new(start, end);
        let normalized = range.normalized();
        assert_eq!(
            normalized.start, start,
            "already-ordered same-row range should not be swapped"
        );
        assert_eq!(normalized.end, end);
    }

    #[test]
    fn test_pinned_range_normalized_preserves_generation() {
        let start = Pin::from_absolute(20, 5, 42);
        let end = Pin::from_absolute(10, 0, 99);
        let range = PinnedRange::new(start, end);
        let normalized = range.normalized();
        // After swap, start should be the old end (gen 99), end should be old start (gen 42)
        assert_eq!(normalized.start.generation(), 99);
        assert_eq!(normalized.end.generation(), 42);
    }

    #[test]
    fn test_pinned_range_normalized_is_idempotent() {
        let start = Pin::from_absolute(20, 5, 0);
        let end = Pin::from_absolute(10, 0, 0);
        let range = PinnedRange::new(start, end);
        let once = range.normalized();
        let twice = once.normalized();
        assert_eq!(once.start, twice.start);
        assert_eq!(once.end, twice.end);
    }

    // Integration: multi-step eviction scenarios

    #[test]
    fn test_sequential_evictions_accumulate() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);

        tracker.evict_page(0);
        tracker.evict_page(1);
        tracker.evict_page(2);

        assert_eq!(tracker.page_generation(0), 1);
        assert_eq!(tracker.page_generation(1), 1);
        assert_eq!(tracker.page_generation(2), 1);
        assert_eq!(tracker.current_generation(), 3);
    }

    #[test]
    fn test_mixed_evict_page_and_evict_all() {
        let mut tracker = GenerationTracker::new();
        tracker.ensure_capacity(3);

        tracker.evict_page(0);
        // Page 0 = gen 1, pages 1,2 = gen 0, global = 1
        let pin_p0 = Pin::new(0, 0, 0, tracker.page_generation(0));
        let pin_p1 = Pin::new(1, 0, 0, tracker.page_generation(1));

        assert!(tracker.is_valid(&pin_p0));
        assert!(tracker.is_valid(&pin_p1));

        tracker.evict_all();
        // All page gens incremented, min_valid raised to global
        assert!(!tracker.is_valid(&pin_p0));
        assert!(!tracker.is_valid(&pin_p1));

        // A fresh evict_page lifts page 1 back to min_valid_generation.
        tracker.evict_page(1);
        let new_pin = Pin::new(1, 0, 0, tracker.page_generation(1));
        assert!(
            tracker.is_valid(&new_pin),
            "pin created after fresh evict_page should be valid"
        );
    }
}
