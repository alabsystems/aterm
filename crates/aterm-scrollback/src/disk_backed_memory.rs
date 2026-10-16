// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory-accounting helpers for [`DiskBackedScrollback`].

use super::*;

impl DiskBackedScrollback {
    /// Get the hot+warm memory usage (bytes); the memory-mapped cold tier is
    /// not counted.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Get the cold tier's in-memory metadata and cache usage (bytes).
    #[must_use]
    pub(crate) fn cold_memory_used(&self) -> usize {
        self.cold.memory_used()
    }

    /// Get the cold tier's compressed size on disk (bytes).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn cold_disk_used(&self) -> usize {
        self.cold.compressed_size()
    }

    /// Get total memory usage across all tiers (bytes): the cold tier's
    /// metadata and cache count, its disk does not.
    #[must_use]
    pub(crate) fn total_memory_used(&self) -> usize {
        // Saturating, as `Scrollback::memory_used` and `Line::memory_used` are:
        // exact whenever the sum fits, `usize::MAX` otherwise.
        self.bytes_used.saturating_add(self.cold_memory_used())
    }

    /// Get reclaimable hot+warm storage bytes for budget enforcement. Excludes
    /// the cold cache and metadata, so read-only cache fills move
    /// [`total_memory_used`](Self::total_memory_used) but not this.
    #[must_use]
    #[inline]
    pub(crate) fn budgeted_memory_used(&self) -> usize {
        self.budgeted_bytes
    }

    /// Reclaimable hot+warm storage bytes (budget enforcement only).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn budgeted_bytes(&self) -> usize {
        self.budgeted_memory_used()
    }

    /// Check if reclaimable hot+warm storage (`budgeted_bytes`, not the
    /// diagnostic `bytes_used`) exceeds the budget.
    #[must_use]
    #[inline]
    pub(crate) fn over_budget(&self) -> bool {
        self.budgeted_bytes > self.memory_budget
    }

    /// Get the current memory pressure watermark level.
    #[must_use]
    #[inline]
    pub fn watermark_level(&self) -> WatermarkLevel {
        self.watermark_level
    }

    /// Recompute watermark level with hysteresis.
    #[inline]
    pub(super) fn update_watermark_level(&mut self) {
        self.watermark_level = super::recompute_watermark(
            self.watermark_level,
            self.budgeted_bytes,
            self.red_threshold,
            self.yellow_threshold,
            self.yellow_exit_threshold,
        );
    }
}
