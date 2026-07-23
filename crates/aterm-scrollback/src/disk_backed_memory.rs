// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory-accounting helpers for [`DiskBackedScrollback`].

use super::*;

impl DiskBackedScrollback {
    /// Get the hot+warm memory usage (bytes).
    ///
    /// Note: Cold tier is disk-backed (memory-mapped) so not counted in RAM usage.
    /// Use [`cold_memory_used`](Self::cold_memory_used) for consistency with [`Scrollback`].
    #[must_use]
    pub(crate) fn memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Get the cold tier memory usage (bytes).
    ///
    /// Returns in-memory metadata + cache usage for the cold tier.
    /// Disk usage is reported by [`cold_disk_used`](Self::cold_disk_used).
    #[must_use]
    pub(crate) fn cold_memory_used(&self) -> usize {
        self.cold.memory_used()
    }

    /// Get the cold tier disk usage (bytes, compressed).
    ///
    /// Returns the compressed size of data stored on disk in the cold tier.
    /// For [`Scrollback`] (in-memory), use [`cold_memory_used`](Scrollback::cold_memory_used) instead.
    #[must_use]
    pub(crate) fn cold_disk_used(&self) -> usize {
        self.cold.compressed_size()
    }

    /// Get total memory usage across all tiers (bytes).
    ///
    /// For [`DiskBackedScrollback`], includes cold tier metadata/cache memory but
    /// excludes disk usage. Use [`cold_disk_used`](Self::cold_disk_used) for disk usage.
    #[must_use]
    pub(crate) fn total_memory_used(&self) -> usize {
        self.bytes_used + self.cold_memory_used()
    }

    /// Get reclaimable storage bytes used for budget enforcement.
    ///
    /// For [`DiskBackedScrollback`], this tracks hot+warm storage only and
    /// excludes cold-tier cache and metadata. Read-only cold-cache fills can
    /// therefore change [`total_memory_used`](Self::total_memory_used) without
    /// perturbing this budget signal.
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

    /// Check if reclaimable hot+warm storage exceeds the budget.
    ///
    /// Uses `budgeted_bytes` (reclaimable storage only), not `bytes_used`
    /// (diagnostic, includes cache and metadata).
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
