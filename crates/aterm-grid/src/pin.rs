// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pin and generation tracking helpers for stable grid references.

use super::page::PageId;

/// Generation counter for detecting stale pins.
pub type Generation = u64;

/// A stable reference to a position in the terminal buffer.
#[cfg(any(test, debug_assertions))]
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

    /// Invalidate all pins by updating the minimum valid generation.
    pub(crate) fn evict_all(&mut self) {
        for generation in &mut self.generations {
            *generation += 1;
        }
        self.global_generation += 1;
        self.min_valid_generation = self.global_generation;
    }
}

/// A pinned range (start and end pins).
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedRange {
    /// Start of the range (inclusive).
    pub start: Pin,
    /// End of the range (inclusive).
    pub end: Pin,
}
