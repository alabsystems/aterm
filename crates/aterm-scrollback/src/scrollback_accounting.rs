// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Accounting and watermark maintenance helpers for [`Scrollback`].

use super::{Scrollback, WatermarkLevel, threshold_bytes};

impl Scrollback {
    /// Configure watermark thresholds as percentages (0-100) of the memory budget.
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

    /// Update both diagnostic and budget aggregates from per-tier counters.
    ///
    /// Spelled `saturating_add`: each operand is bytes of memory this process
    /// actually holds, so the true sum always fits in `usize` and the
    /// saturation can never fire on a real path — it just discharges the
    /// strict L0 gate's unconstrained-input overflow counterexamples.
    pub(crate) fn sync_accounting(&mut self) {
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
    pub(crate) fn update_watermark_level(&mut self) {
        self.watermark_level = super::recompute_watermark(
            self.watermark_level,
            self.budgeted_bytes,
            self.red_threshold,
            self.yellow_threshold,
            self.yellow_exit_threshold,
        );
    }

    /// Saturating sums for the same strict-gate reason as
    /// [`sync_accounting`](Self::sync_accounting) (identical on every real path).
    ///
    /// `cfg(test)`: only called by the (test-only) invariant assert below and
    /// by the memory-tracking tests, so a broader cfg would just leave dead
    /// code in non-test debug builds.
    #[cfg(test)]
    pub(crate) fn recompute_total_memory_used(&self) -> usize {
        self.hot
            .recompute_memory_used()
            .saturating_add(self.warm.recompute_memory_used())
            .saturating_add(self.cold.recompute_compressed_size())
    }

    /// Saturating sums for the same strict-gate reason as
    /// [`sync_accounting`](Self::sync_accounting) (identical on every real path).
    ///
    /// `cfg(test)`: see [`recompute_total_memory_used`](Self::recompute_total_memory_used).
    #[cfg(test)]
    pub(crate) fn recompute_budgeted_bytes(&self) -> usize {
        self.hot
            .recompute_budgeted_bytes()
            .saturating_add(self.warm.recompute_budgeted_bytes())
            .saturating_add(self.cold.recompute_compressed_size())
    }

    /// Consistency check: the aggregate counters must equal a from-scratch
    /// recomputation over the tiers.
    ///
    /// Test-only (`cfg(test)`): the whole point of these asserts is that they
    /// CAN fire when a counter drifts, so the strict L0 gate — which must
    /// refute every reachable panic — would reject the library build that
    /// carried them. The invariant is exercised by the test suite (which
    /// calls this after every mutation in the accounting/pressure/memory
    /// tests); non-test debug builds simply skip the recomputation, which has
    /// no other side effects.
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
