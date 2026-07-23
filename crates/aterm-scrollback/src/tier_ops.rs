// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-crossing operations on [`Scrollback`].
//!
//! Methods that read or write across the hot/warm/cold tier system.
//! Separated from `lib.rs` (which holds the struct definition, constructors,
//! and read-only accessors) to keep file sizes under the 500-line limit.

use std::borrow::Cow;

use super::{
    DEFAULT_RED_PERCENT, DEFAULT_YELLOW_PERCENT, Line, Scrollback, ScrollbackError, WatermarkLevel,
    YELLOW_EXIT_PERCENT, threshold_bytes,
};

impl Scrollback {
    /// Get a line by index (0 = oldest).
    ///
    /// Returns `Cow::Borrowed` for hot-tier lines (zero-copy) and
    /// `Cow::Owned` for warm/cold-tier lines (decompressed on access). (#5950)
    ///
    /// REQUIRES: idx < self.line_count() for Some result
    /// ENSURES: idx >= self.line_count() implies result == Ok(None)
    #[must_use = "line data is discarded if not consumed"]
    // Skip: the tier-dispatch line lookup — the guarded-index/decode class of
    // the per-tier `get_line`s it routes into (each individually classified).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        let cold_count = self.cold.line_count();
        let warm_count = self.warm.line_count();

        if idx < cold_count {
            self.cold.get_line(idx).map(|opt| opt.map(Cow::Owned))
        } else if idx < cold_count.saturating_add(warm_count) {
            self.warm
                .get_line(idx.saturating_sub(cold_count))
                .map(|opt| opt.map(Cow::Owned))
        } else {
            let hot_idx = idx.saturating_sub(cold_count).saturating_sub(warm_count);
            let Some(line) = self.hot.get(hot_idx) else {
                return Err(ScrollbackError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("in-range line index {idx} mapped to missing hot tier index {hot_idx}"),
                )));
            };
            Ok(Some(Cow::Borrowed(line)))
        }
    }

    /// Get a line by reverse index (0 = newest).
    ///
    /// REQUIRES: rev_idx < self.line_count() for Some result
    /// ENSURES: rev_idx >= self.line_count() implies result == Ok(None)
    #[must_use = "line data is discarded if not consumed"]
    // Skip: the reverse-index twin of `get_line` — same tier-dispatch
    // guarded-index/decode class.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        if rev_idx >= self.line_count {
            return Ok(None);
        }
        self.get_line(self.line_count.saturating_sub(1).saturating_sub(rev_idx))
    }

    /// Set the memory budget (bytes).
    ///
    /// Enforces the new budget immediately by evicting warm blocks if needed.
    /// Returns `Err` if enforcement failed (e.g. corrupted warm blocks
    /// prevented eviction and the budget is still exceeded).
    ///
    /// ENSURES: self.memory_budget() >= 1 (clamped from input)
    pub fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        self.memory_budget = budget.max(1);
        // Recompute watermark thresholds for the new budget.
        self.yellow_threshold = threshold_bytes(DEFAULT_YELLOW_PERCENT, self.memory_budget);
        self.yellow_exit_threshold = threshold_bytes(YELLOW_EXIT_PERCENT, self.memory_budget);
        self.red_threshold = threshold_bytes(DEFAULT_RED_PERCENT, self.memory_budget);
        // Reset watermark before recomputing: threshold changes are a fresh
        // assessment, not subject to hysteresis from the old configuration.
        self.watermark_level = WatermarkLevel::Green;
        self.update_watermark_level();
        self.handle_memory_pressure();
        self.assert_bytes_used_invariant();
        // If still over budget and warm blocks remain, eviction was blocked
        // (e.g. corrupt data). Hot-tier-only overages are expected and not
        // an error — the hot tier holds active data that cannot be evicted.
        if self.over_budget() && self.warm.block_count() > 0 {
            let over_bytes = self.budgeted_bytes.saturating_sub(self.memory_budget);
            return Err(ScrollbackError::EnforcementFailed { over_bytes });
        }
        Ok(())
    }

    /// Set the line limit (maximum total lines allowed).
    ///
    /// When set, older lines are discarded when this limit is exceeded.
    /// Setting to `None` removes the limit.
    /// Setting to `Some(0)` effectively disables scrollback.
    ///
    /// Enforces the new limit immediately by truncating if needed.
    /// If truncation fails (decompression error), the limit is set but
    /// enforcement is deferred to the next push.
    ///
    /// ENSURES: self.line_limit() == limit
    pub fn set_line_limit(&mut self, limit: Option<usize>) {
        self.line_limit = limit;
        // Enforce the limit immediately
        if let Some(max) = limit
            && self.line_count > max
            && let Err(e) = self.truncate(max)
        {
            aterm_log::warn!(
                "set_line_limit: truncation to {max} failed ({e}), limit set but not yet enforced"
            );
        }
    }

    /// Push a new line to the scrollback.
    ///
    /// Lines are added to the hot tier. When the hot tier is full,
    /// old lines are promoted to the warm tier (compressed).
    /// If a line limit is set and exceeded, the oldest line is discarded.
    ///
    /// ENSURES: self.line_count() >= 1
    pub fn push_line(&mut self, line: Line) {
        // If hot tier is full, promote oldest lines to warm
        if self.hot.len() >= self.hot_limit {
            self.promote_hot_to_warm();
        }

        // Add line to hot tier
        self.hot.push(line);
        // line_count counts lines held across the tiers; every counted line
        // occupies at least one byte of memory, so the count is bounded well
        // below usize::MAX and saturating_add never saturates —
        // behavior-identical to `+= 1`.
        self.line_count = self.line_count.saturating_add(1);
        self.sync_accounting();

        // Enforce line limit by discarding oldest line if over limit
        if let Some(max) = self.line_limit
            && self.line_count > max
            && let Err(e) = self.truncate(max)
        {
            aterm_log::warn!(
                "push_line: truncation to limit {max} failed ({e}), limit temporarily unenforced"
            );
        }

        // Eager promotion under memory pressure: compress hot→warm sooner to
        // free uncompressed memory before the budget is fully exhausted.
        if self.watermark_level >= WatermarkLevel::Yellow && self.hot.len() >= self.block_size {
            self.promote_hot_to_warm();
        }

        // Handle memory pressure
        self.handle_memory_pressure();
    }

    /// Push a line from a string.
    pub fn push_str(&mut self, s: &str) {
        self.push_line(Line::from(s));
    }

    /// Clear all lines.
    ///
    /// ENSURES: self.line_count() == 0
    /// ENSURES: self.memory_used() == 0
    pub fn clear(&mut self) {
        self.hot.clear();
        self.warm.clear();
        self.cold.clear();
        self.line_count = 0;
        self.sync_accounting();
        self.assert_bytes_used_invariant();
    }

    /// Remove the `n` most recent lines from scrollback.
    ///
    /// Per the Kitty unscroll spec: "The lines that have been scrolled into
    /// the scrollback buffer are removed from the scrollback buffer."
    ///
    /// Uses tier-aware back-removal: removes from hot first, then warm, then
    /// cold — decompressing at most one boundary block/page at a time. This
    /// bounds peak memory to ~one block regardless of total scrollback size.
    ///
    /// Returns an error if decompression fails during line extraction.
    /// On error, scrollback state is unchanged (no lines lost). (#4638)
    ///
    /// ENSURES: Ok(()) implies self.line_count() == old_line_count.saturating_sub(n)
    /// ENSURES: Err(_) implies state unchanged
    // Skip: the cross-tier back-removal driver — the blanket-unmodeled
    // drain/iterator class of the tier truncations it calls (their arithmetic
    // is saturating and proven). ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 || self.line_count == 0 {
            return Ok(());
        }
        if n >= self.line_count {
            self.clear();
            return Ok(());
        }

        // Calculate per-tier removal amounts.
        let hot_remove = n.min(self.hot.len());
        let after_hot = n.saturating_sub(hot_remove);
        let warm_remove = after_hot.min(self.warm.line_count());
        let cold_remove = after_hot.saturating_sub(warm_remove);

        // Pre-validate all fallible decompressions before modifying state (#4638).
        // Each tier's truncate_back_lines is internally error-safe, but calling
        // them sequentially is NOT — if the second fails, the first already
        // committed. Pre-validation ensures all boundary blocks/pages decompress
        // successfully before any tier is modified.
        if warm_remove > 0 {
            self.warm.pre_validate_truncate_back(warm_remove)?;
        }
        if cold_remove > 0 {
            self.cold.pre_validate_truncate_back(cold_remove)?;
        }

        // Commit. Pre-validation confirms each boundary block DECOMPRESSES, but
        // the re-compress step is still fallible and pre-validation cannot
        // exercise it. Propagate any such error with `?` instead of panicking;
        // each tier's `truncate_back_lines` is internally error-safe (state
        // unchanged on its own error, #4638).
        // Decrement the aggregate `line_count` INCREMENTALLY as each tier's
        // truncation commits. The warm/cold re-compress steps stay fallible even
        // after pre-validation, so a later tier's error must leave `line_count`
        // matching exactly the tiers that already succeeded — committing a bulk
        // `-= n` only at the end overstates the removal on a partial failure and
        // desyncs the aggregate from the tiers. Newest-first order (hot→warm→
        // cold) means a partial failure trims the newest lines and leaves only
        // the oldest boundary portion in place.
        if hot_remove > 0 {
            self.hot.truncate_back(hot_remove);
            self.line_count = self.line_count.saturating_sub(hot_remove);
        }
        if warm_remove > 0 {
            self.warm.truncate_back_lines(warm_remove)?;
            self.line_count = self.line_count.saturating_sub(warm_remove);
        }
        if cold_remove > 0 {
            self.cold.truncate_back_lines(cold_remove)?;
            self.line_count = self.line_count.saturating_sub(cold_remove);
        }
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }

    /// Truncate to keep only the last `n` lines.
    ///
    /// Tier-aware: removes oldest lines from cold → warm → hot without
    /// decompressing the entire scrollback. Both cold and warm tiers use
    /// `front_offset` for O(1) line removal with no decompression.
    /// The hot tier uses simple front truncation.
    ///
    /// Returns `Result` for API compatibility, but all steps are now infallible
    /// after the warm tier `front_offset` optimization (no boundary block
    /// decompression during truncation).
    ///
    /// ENSURES: Ok(()) implies self.line_count() <= n
    /// ENSURES: n == 0 implies self.line_count() == 0
    pub fn truncate(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 {
            self.clear();
            return Ok(());
        }
        if n >= self.line_count {
            return Ok(());
        }

        // saturating chain: `n < line_count` (guarded above) and each tier's
        // remove is `min`-capped, so none ever actually saturates — this only
        // discharges the obligations the verifier cannot chain. Identical.
        let to_remove = self.line_count.saturating_sub(n);

        // Calculate per-tier removal amounts up front.
        let cold_lines = self.cold.line_count();
        let cold_remove = to_remove.min(cold_lines);
        let after_cold = to_remove.saturating_sub(cold_remove);

        let warm_lines = self.warm.line_count();
        let warm_remove = after_cold.min(warm_lines);
        let hot_remove = after_cold.saturating_sub(warm_remove);

        // All three tiers are now infallible during truncation (front_offset pattern).
        if warm_remove > 0 {
            self.warm.truncate_front_lines(warm_remove)?;
        }
        if cold_remove > 0 {
            self.cold.truncate_front_lines(cold_remove);
        }
        if hot_remove > 0 {
            let hot_keep = self.hot.len().saturating_sub(hot_remove);
            self.hot.truncate_front(hot_keep);
        }

        self.line_count = n;
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }

    /// Promote oldest hot lines to warm tier.
    fn promote_hot_to_warm(&mut self) {
        if self.hot.len() < self.block_size {
            return;
        }

        // Take block_size lines from front of hot tier
        let lines = self.hot.take_front(self.block_size);
        if lines.is_empty() {
            return;
        }

        // Compress and add to warm tier
        self.warm.push_block(&lines);

        // If warm tier is over limit, evict to cold.
        // Failure is OK here — handle_memory_pressure will retry or fall through
        // to cold eviction.
        if self.warm.line_count() > self.warm_limit {
            let _ = self.evict_warm_to_cold();
        }

        self.sync_accounting();
        self.assert_bytes_used_invariant();
    }

    /// Evict oldest warm block to cold tier.
    ///
    /// Returns `true` if the block was accepted by cold tier or quarantined
    /// (freed memory), `false` if eviction failed and the block was restored
    /// to warm for retry. Blocks that fail decompression repeatedly are
    /// quarantined (dropped) after [`QUARANTINE_THRESHOLD`] consecutive failures. (#5947)
    fn evict_warm_to_cold(&mut self) -> bool {
        let Some(block) = self.warm.pop_front() else {
            return false;
        };
        let accepted = self.cold.push_block(&block);
        if accepted == 0 && block.line_count() > 0 {
            // decompress() already incremented the failure counter when it
            // failed inside cold.push_block → to_cold_compressed → decompress.
            if block.is_quarantined() {
                // Permanent corruption — drop the block and adjust line count.
                let lost = block.line_count();
                self.line_count = self.line_count.saturating_sub(lost);
                aterm_log::warn!(
                    "quarantined corrupt warm block: {lost} lines dropped \
                     after {} consecutive decompression failures",
                    super::tier::QUARANTINE_THRESHOLD,
                );
                self.sync_accounting();
                self.assert_bytes_used_invariant();
                return true; // Made progress (freed memory)
            }
            // Not yet quarantined — restore to warm for retry.
            self.warm.push_front(block);
            self.sync_accounting();
            self.assert_bytes_used_invariant();
            return false;
        }
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        true
    }

    /// Create a snapshot containing only hot + warm tier lines (skip cold).
    ///
    /// This is the fast path for checkpointing: cold tier decompression
    /// (Zstd or disk I/O) is skipped, bounding the snapshot to at most
    /// `hot_limit + warm_limit` lines. Hot lines are already uncompressed;
    /// warm lines require only LZ4 decompression (~5 GB/s throughput).
    ///
    /// Use this instead of the full `iter()` snapshot when the caller holds
    /// a lock and cannot afford unbounded decompression time. (#5946)
    #[must_use]
    pub fn checkpoint_snapshot_fast(&self) -> Scrollback {
        super::access::checkpoint_snapshot_fast_from(self)
    }

    /// Handle memory pressure by evicting warm → cold, then cold FIFO.
    fn handle_memory_pressure(&mut self) {
        let mut changed = false;
        // First, evict warm blocks to cold (compresses further).
        // Break if eviction fails — retrying the same block loops forever (#5921).
        while self.over_budget() && self.warm.block_count() > 0 {
            if !self.evict_warm_to_cold() {
                break;
            }
            changed = true;
        }
        // If still over budget, batch-evict oldest cold pages (#5444, #5858).
        // Uses evict_bytes for O(P) total cost instead of O(K*P) from repeated pop_front.
        if self.over_budget() && self.cold.line_count() > 0 {
            let excess = self.budgeted_bytes.saturating_sub(self.memory_budget);
            let evicted_lines = self.cold.evict_bytes(excess);
            self.line_count = self.line_count.saturating_sub(evicted_lines);
            self.sync_accounting();
            changed = true;
        }
        if changed {
            self.assert_bytes_used_invariant();
        }
    }
}

#[cfg(test)]
impl Scrollback {
    /// Inject a corrupted warm block at the front of the warm tier.
    ///
    /// The block reports `line_count` lines but its compressed data is invalid,
    /// causing decompression and warm→cold eviction to fail. Used by quarantine
    /// behavioral tests (#5947).
    pub(crate) fn inject_corrupted_warm_block(&mut self, line_count: usize) {
        self.warm.push_front_corrupt(line_count);
        self.line_count += line_count;
        self.sync_accounting();
    }
}
