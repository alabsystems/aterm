// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Front-truncation operations for [`WarmTier`].
//!
//! Extracted from `tier.rs` for file-size compliance (#5947).
//! Supports `Scrollback::truncate` which removes oldest lines.
//!
//! Uses `front_offset` for O(1) line removal: instead of decompressing
//! the boundary block to trim a few lines, we advance the offset counter.
//! Blocks are dropped when fully consumed. This mirrors the cold tier's
//! `front_offset` pattern and eliminates LZ4 decompression from the
//! push-truncate hot path.

use super::WarmTier;

impl WarmTier {
    /// Materialize front_offset by trimming the first block in place.
    ///
    /// Called before operations that need the front block to contain only
    /// live (non-consumed) lines: pop_front, push_front (restoring after
    /// failed eviction). O(1) when front_offset == 0.
    ///
    /// If the front block is corrupt and cannot be decompressed, we still
    /// convert it into a logical "surviving suffix" by shrinking its recorded
    /// line count and clearing front_offset. This preserves the remaining live
    /// lines for the normal warm eviction/quarantine flow instead of dropping
    /// them immediately.
    pub(super) fn materialize_front_offset(&mut self) {
        if self.front_offset == 0 || self.blocks.is_empty() {
            return;
        }

        while let Some(front) = self.blocks.front() {
            if self.front_offset < front.line_count() {
                break;
            }

            // Fully consumed block — front_offset already removed these lines
            // from the logical count, so only the compressed storage changes.
            //
            // `let-else` + break instead of `.expect(..)`: the loop condition
            // just witnessed a front block, so the `None` arm is unreachable
            // and behavior is identical — but the strict L0 gate must refute
            // every reachable panic, and it cannot carry the witness across
            // the `pop_front` call.
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
            self.rebuild_cumulative();
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
                // saturating on BOTH sides: the sub already saturates; the add
                // must too (a byte total cannot overflow usize for real data —
                // exact on every real path).
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
    /// Advances `front_offset` by `n` and drops any blocks that become fully
    /// consumed. O(1) when no block boundary is crossed; O(blocks_dropped) when
    /// blocks are consumed. No decompression is performed.
    ///
    /// This replaces the previous decompress-trim-recompress approach, making
    /// line-limit enforcement during `push_line` O(1) instead of O(block_size).
    ///
    /// # Panics
    ///
    /// Debug-asserts that `n <= self.line_count` (test builds only; production
    /// builds warn and saturate instead — see the contract note in the body).
    pub(crate) fn truncate_front_lines(&mut self, n: usize) -> Result<(), crate::ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        // Caller contract (`n <= self.line_count`) is checked under
        // `cfg(test)` only: a contract assert is a reachable panic the strict
        // L0 gate must refute and cannot; the warn-and-saturate fallback below
        // is the production behavior either way, and the test suite (which
        // runs with `cfg(test)`) still catches contract violations.
        #[cfg(test)]
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            // Pre-composed message via the log shim: identical rendered
            // record, but no macro-expanded `format_args!` unsafe in THIS
            // function (which the strict gate would escalate — see log_shim.rs).
            let mut msg = String::from("warm truncate_front_lines(");
            msg.push_str(&crate::error::dec_string(n));
            msg.push_str(") exceeds line_count(");
            msg.push_str(&crate::error::dec_string(self.line_count));
            msg.push_str("), saturating");
            crate::log_shim::warn_str(&msg);
        }

        // Saturating: `front_offset <= line_count` (both bounded by lines this
        // process actually stored), so the sum always fits in `usize` and the
        // saturation can never fire on a real path — it just discharges the
        // strict L0 gate's unconstrained-input overflow counterexample.
        self.front_offset = self.front_offset.saturating_add(n);
        self.line_count = self.line_count.saturating_sub(n);

        // Drop fully consumed front blocks. A bool (`dropped_any`) replaces
        // the previous `blocks_dropped` counter: it was only compared `> 0`,
        // and the flag has no increment for the gate to refute.
        let mut dropped_any = false;
        while let Some(front) = self.blocks.front() {
            if self.front_offset >= front.line_count() {
                // `let-else` + break instead of `.expect(..)`: the loop
                // condition just witnessed a front block, so the `None` arm is
                // unreachable and behavior is identical (see
                // materialize_front_offset).
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
                dropped_any = true;
            } else {
                break;
            }
        }

        if dropped_any {
            // Rebuild cumulative index after removing front blocks.
            self.rebuild_cumulative();
            // Invalidate cache — block indices shifted.
            self.clear_cache();
        }

        Ok(())
    }
}
