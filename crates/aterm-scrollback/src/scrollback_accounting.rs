// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Accounting and watermark maintenance helpers for [`Scrollback`].

use super::Scrollback;
#[cfg(test)]
use super::{WatermarkLevel, threshold_bytes};

impl Scrollback {
    /// Configure watermark thresholds as percentages (0-100) of the memory budget.
    #[cfg(test)]
    pub fn set_watermark_thresholds(&mut self, yellow_percent: usize, red_percent: usize) {
        let yellow = yellow_percent.clamp(1, 100);
        let red = red_percent.clamp(yellow, 100);
        let exit = yellow / 2;

        self.yellow_threshold = threshold_bytes(yellow, self.memory_budget);
        self.yellow_exit_threshold = threshold_bytes(exit.max(1), self.memory_budget);
        self.red_threshold = threshold_bytes(red, self.memory_budget);
        self.watermark_level = WatermarkLevel::Green;
        self.update_watermark_level();
    }

    /// Monotonic count of lines dropped by memory-pressure eviction (the byte
    /// budget's cold-tier FIFO) or lost with a quarantined warm block: real
    /// retention loss, reported out of band (no sentinel content is injected).
    /// Each line counts once: the blank rows a quarantined block leaves in cold
    /// are not counted again when the budget evicts them. `set_line_limit`
    /// truncation is intentional and not counted.
    #[must_use]
    #[inline]
    pub const fn pressure_evicted_lines(&self) -> u64 {
        self.pressure_evicted_lines
    }

    /// Monotonic count of history lines whose inline image was discarded because
    /// the line was compressed: the out-of-band signal for the image retention
    /// horizon, the sibling of [`pressure_evicted_lines`](Self::pressure_evicted_lines).
    ///
    /// An image lives only while its line is uncompressed in the hot tier; the
    /// warm and cold wire format has no image section (see `Line::serialize`).
    /// The line keeps its text, attributes, hyperlinks and underline colours, so
    /// a footprint ages out one row at a time from its top. The count is in
    /// footprint rows (one per history line); it never decreases and
    /// [`clear`](Self::clear) does not reset it.
    #[must_use]
    #[inline]
    pub const fn image_rows_dropped_by_compression(&self) -> u64 {
        self.image_rows_dropped_by_compression
    }

    /// Update both diagnostic and budget aggregates from per-tier counters.
    ///
    /// Saturating: each operand is bytes this process holds, so the true sum
    /// fits; saturation only discharges the strict L0 gate's unconstrained-input
    /// overflow counterexamples.
    #[inline]
    pub(crate) const fn sync_accounting(&mut self) {
        self.bytes_used = self
            .hot
            .memory_used()
            .saturating_add(self.warm.memory_used())
            .saturating_add(self.cold.compressed_size());
        self.budgeted_bytes = self
            .hot
            .budgeted_bytes()
            .saturating_add(self.warm.budgeted_bytes())
            .saturating_add(self.cold.compressed_size());
        self.update_watermark_level();
    }

    /// Recompute watermark level from current `budgeted_bytes` vs thresholds.
    #[inline]
    pub(crate) const fn update_watermark_level(&mut self) {
        self.watermark_level = super::recompute_watermark(
            self.watermark_level,
            self.budgeted_bytes,
            self.red_threshold,
            self.yellow_threshold,
            self.yellow_exit_threshold,
        );
    }

    /// Saturating for the Trust lane, as in [`sync_accounting`](Self::sync_accounting).
    /// Test-only: only the invariant assert and the memory-tracking tests call it.
    #[cfg(test)]
    pub(crate) fn recompute_total_memory_used(&self) -> usize {
        self.hot
            .recompute_memory_used()
            .saturating_add(self.warm.recompute_memory_used())
            .saturating_add(self.cold.recompute_compressed_size())
    }

    /// Saturating for the Trust lane, as in [`sync_accounting`](Self::sync_accounting).
    #[cfg(test)]
    pub(crate) fn recompute_budgeted_bytes(&self) -> usize {
        self.hot
            .recompute_budgeted_bytes()
            .saturating_add(self.warm.recompute_budgeted_bytes())
            .saturating_add(self.cold.recompute_compressed_size())
    }

    /// Consistency check: the aggregate counters equal a from-scratch
    /// recomputation over the tiers.
    ///
    /// The asserts run only under `cfg(test)`: they can fire when a counter
    /// drifts, and the strict L0 gate, which refutes every reachable panic,
    /// would reject a library build that carried them.
    pub(crate) fn assert_bytes_used_invariant(&self) {
        #[cfg(test)]
        {
            debug_assert_eq!(
                self.bytes_used,
                self.recompute_total_memory_used(),
                "scrollback bytes_used counter drift",
            );
            debug_assert_eq!(
                self.budgeted_bytes,
                self.recompute_budgeted_bytes(),
                "scrollback budgeted_bytes counter drift",
            );
            let tier_line_count = self
                .hot
                .len()
                .saturating_add(self.warm.line_count())
                .saturating_add(self.cold.line_count());
            debug_assert_eq!(
                self.line_count, tier_line_count,
                "scrollback line_count drift: aggregate={} but tiers sum={}",
                self.line_count, tier_line_count,
            );
        }
    }
}
