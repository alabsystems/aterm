// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Front truncation for [`WarmTier`], backing `Scrollback::truncate`.
//!
//! The oldest lines are removed by advancing `front_offset` rather than
//! decompressing the boundary block; blocks drop once fully consumed, as in the
//! cold tier, so the push-truncate hot path never decodes.

use super::WarmTier;

impl WarmTier {
    /// Materialize front_offset by trimming the first block in place.
    ///
    /// Called before operations that need the front block to hold only live
    /// lines (pop_front, push_front after a failed eviction). O(1) when
    /// front_offset == 0. A front block that cannot be decompressed becomes a
    /// logical surviving suffix (its line count shrinks, front_offset clears),
    /// so its live lines go through the normal eviction/quarantine flow.
    pub(super) fn materialize_front_offset(&mut self) {
        if self.front_offset == 0 || self.blocks.is_empty() {
            return;
        }

        let mut blocks_dropped: usize = 0;
        while let Some(front) = self.blocks.front() {
            if self.front_offset < front.line_count() {
                break;
            }

            // Fully consumed block: front_offset already removed these lines
            // from the logical count, so only the storage changes. `let-else`
            // rather than `expect`: the strict L0 gate cannot carry the loop
            // condition's witness across `pop_front`.
            let Some(block) = self.blocks.pop_front() else {
                break;
            };
            self.budgeted_bytes = self.budgeted_bytes.saturating_sub(block.compressed_size());
            let bytes_used = self
                .bytes_used
                .get()
                .saturating_sub(block.compressed_size());
            self.bytes_used.set(bytes_used);
            self.front_offset = self.front_offset.saturating_sub(block.line_count());
            blocks_dropped = blocks_dropped.saturating_add(1);
        }

        if blocks_dropped > 0 {
            self.drop_front_index_entries(blocks_dropped);
            self.clear_cache();
        }

        if self.front_offset == 0 || self.blocks.is_empty() {
            return;
        }

        let front = &self.blocks[0];
        match front.decompress() {
            Ok(lines) => {
                // `get` (not range-index): `front_offset <= lines.len()` by
                // invariant, but the verifier cannot chain it — the None arm is
                // unreachable and yields the same empty-survivor rebuild.
                let surviving = lines.get(self.front_offset..).unwrap_or(&[]);
                let old_size = self.blocks[0].compressed_size();
                let replacement = super::WarmBlock::from_lines(surviving);
                let new_size = replacement.compressed_size();
                self.blocks[0] = replacement;
                // Saturating both ways; exact on every real path.
                self.budgeted_bytes = self
                    .budgeted_bytes
                    .saturating_sub(old_size)
                    .saturating_add(new_size);
                let bytes_used = self
                    .bytes_used
                    .get()
                    .saturating_sub(old_size)
                    .saturating_add(new_size);
                self.bytes_used.set(bytes_used);
                self.front_offset = 0;
                self.rebuild_cumulative();
                self.clear_cache();
            }
            Err(_) => {
                // Keep the surviving suffix logically present so eviction can
                // retry and eventually quarantine only the remaining live lines.
                let surviving = self.blocks[0]
                    .line_count()
                    .saturating_sub(self.front_offset);
                self.blocks[0].line_count = surviving;
                self.front_offset = 0;
                self.rebuild_cumulative();
                self.clear_cache();
            }
        }
    }

    /// Remove the oldest `n` lines from the front of the warm tier.
    ///
    /// Advances `front_offset` by `n` and drops blocks that become fully
    /// consumed, without decompressing: O(1) unless a block boundary is crossed.
    ///
    /// # Panics
    ///
    /// Debug-asserts `n <= self.line_count` in test builds; other builds warn
    /// and saturate.
    pub(crate) fn truncate_front_lines(&mut self, n: usize) -> Result<(), crate::ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        // The caller contract is asserted only under `cfg(test)`: an assert is
        // a reachable panic the strict L0 gate cannot refute, and
        // warn-and-saturate is the behavior either way.
        #[cfg(test)]
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            // Pre-composed via the log shim, for the Trust lane (see log_shim.rs).
            let mut msg = String::from("warm truncate_front_lines(");
            msg.push_str(&crate::error::dec_string(n));
            msg.push_str(") exceeds line_count(");
            msg.push_str(&crate::error::dec_string(self.line_count));
            msg.push_str("), saturating");
            crate::log_shim::warn_str(&msg);
        }

        // Saturating: `front_offset <= line_count`, so the sum fits; saturation
        // only discharges the strict L0 gate's overflow counterexample.
        self.front_offset = self.front_offset.saturating_add(n);
        self.line_count = self.line_count.saturating_sub(n);

        // Drop fully consumed front blocks and advance the cumulative index
        // in O(1) amortized instead of O(B) per truncate.
        let mut blocks_dropped: usize = 0;
        while let Some(front) = self.blocks.front() {
            if self.front_offset >= front.line_count() {
                // `let-else`, as in materialize_front_offset.
                let Some(block) = self.blocks.pop_front() else {
                    break;
                };
                self.front_offset = self.front_offset.saturating_sub(block.line_count());
                self.budgeted_bytes = self.budgeted_bytes.saturating_sub(block.compressed_size());
                let bytes_used = self
                    .bytes_used
                    .get()
                    .saturating_sub(block.compressed_size());
                self.bytes_used.set(bytes_used);
                blocks_dropped = blocks_dropped.saturating_add(1);
            } else {
                break;
            }
        }

        if blocks_dropped > 0 {
            self.drop_front_index_entries(blocks_dropped);
            // Invalidate cache — block indices shifted.
            self.clear_cache();
        }

        Ok(())
    }
}
