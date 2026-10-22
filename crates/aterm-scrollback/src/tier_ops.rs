// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-crossing operations on [`Scrollback`]: methods that read or write
//! across the hot/warm/cold tiers (`lib.rs` holds the struct, constructors and
//! read-only accessors).

use std::borrow::Cow;

use super::{
    DEFAULT_RED_PERCENT, DEFAULT_YELLOW_PERCENT, Line, Scrollback, ScrollbackError, WatermarkLevel,
    YELLOW_EXIT_PERCENT, threshold_bytes,
};

impl Scrollback {
    /// Get a line by index (0 = oldest).
    ///
    /// Returns `Cow::Borrowed` for hot-tier lines (zero-copy) and
    /// `Cow::Owned` for warm/cold-tier lines (decompressed on access).
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
        if self.hot.len() >= self.hot_limit {
            self.promote_hot_to_warm();
        }

        self.hot.push(line);
        // saturating_add never saturates (every counted line holds memory);
        // identical to `+= 1`.
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
    /// Takes the newest lines from hot, then warm, then cold, committing the
    /// oldest affected tier first and decompressing at most one boundary
    /// block/page, so peak memory is ~one block. On a decompression error the
    /// scrollback is unchanged.
    ///
    /// ENSURES: Ok(()) implies self.line_count() == old_line_count.saturating_sub(n)
    /// ENSURES: Err(_) implies state unchanged
    // Skip: the cross-tier back-removal driver — the blanket-unmodeled
    // drain/iterator class of the tier truncations it calls (their arithmetic
    // is saturating and proven).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 || self.line_count == 0 {
            return Ok(());
        }
        if n >= self.line_count {
            self.clear();
            return Ok(());
        }

        let hot_remove = n.min(self.hot.len());
        let after_hot = n.saturating_sub(hot_remove);
        let warm_remove = after_hot.min(self.warm.line_count());
        let cold_remove = after_hot.saturating_sub(warm_remove);

        // Commit the oldest affected tier first: only it can have a partial
        // boundary, hence the only fallible rebuild; every newer tier goes whole.
        // Each truncation prepares its boundary before mutating, so a failure
        // leaves the scrollback unchanged.
        if cold_remove > 0 {
            self.cold.truncate_back_lines(cold_remove)?;
        }
        if warm_remove > 0 {
            self.warm.truncate_back_lines(warm_remove)?;
        }
        if hot_remove > 0 {
            self.hot.truncate_back(hot_remove);
        }
        self.line_count = self.line_count.saturating_sub(n);
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }

    /// Truncate to keep only the last `n` lines.
    ///
    /// Removes the oldest lines cold → warm → hot without decompressing: cold
    /// and warm advance a `front_offset`, hot truncates its front. Every step is
    /// infallible; the `Result` is kept for API compatibility.
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

        // Saturating: `n < line_count` and each remove is `min`-capped, so
        // nothing saturates; it discharges obligations the verifier cannot chain.
        let to_remove = self.line_count.saturating_sub(n);

        let cold_lines = self.cold.line_count();
        let cold_remove = to_remove.min(cold_lines);
        let after_cold = to_remove.saturating_sub(cold_remove);

        let warm_lines = self.warm.line_count();
        let warm_remove = after_cold.min(warm_lines);
        let hot_remove = after_cold.saturating_sub(warm_remove);

        // All three are infallible here (front_offset).
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

        let lines = self.hot.take_front(self.block_size);
        if lines.is_empty() {
            return;
        }

        // The image retention horizon: compression is where a line's inline
        // picture ends (no image section on the wire; see `Line::serialize`), so
        // the loss is counted here, before the lines are consumed.
        self.image_rows_dropped_by_compression = self
            .image_rows_dropped_by_compression
            .saturating_add(crate::count_image_rows(&lines));

        self.warm.push_block(&lines);

        // Evict to cold while warm is over its limit: a batch can arrive as
        // several blocks (see `WarmTier::push_block`), and an evicted block can
        // be shorter than the batch. A failure is fine: handle_memory_pressure
        // retries or falls through to cold eviction.
        while self.warm.line_count() > self.warm_limit && self.evict_warm_to_cold() {}

        self.sync_accounting();
        self.assert_bytes_used_invariant();
    }

    /// Evict the oldest warm block to cold.
    ///
    /// Returns `true` if cold accepted it or it was quarantined (memory freed),
    /// `false` if eviction failed and the block went back to warm for retry. A
    /// block is quarantined after `tier::QUARANTINE_THRESHOLD` consecutive
    /// decompression failures.
    fn evict_warm_to_cold(&mut self) -> bool {
        let Some(block) = self.warm.pop_front() else {
            return false;
        };
        let accepted = self.cold.push_block(&block);
        if accepted == 0 && block.line_count() > 0 {
            // decompress() already incremented the failure counter when it
            // failed inside cold.push_block → to_cold_compressed → decompress.
            if block.is_quarantined() {
                // Permanent corruption: the block's text is lost, and counted
                // as the loss it is (`truncated_lines` in the grid reads it).
                let lost = block.line_count();
                self.pressure_evicted_lines =
                    self.pressure_evicted_lines.saturating_add(lost as u64);
                // Its rows stay, blank (what every reader showed for them), when
                // older history sits in cold: dropping them from the middle would
                // give each older line the absolute row of the line `lost` rows
                // newer, a renumbering no fence or mark sees. With nothing older,
                // dropping them is just the oldest history going. If cold refuses
                // even the blank block, the rows leave the middle after all, with
                // no epoch bump.
                // Kept, the blank page is marked LOST: the budget evicting it
                // later must not count these lines a second time.
                let kept = self.cold.line_count() > 0 && {
                    let blank = super::tier::WarmBlock::from_lines(&vec![Line::default(); lost]);
                    self.cold.push_lost_block(&blank) != 0
                };
                if !kept {
                    self.line_count = self.line_count.saturating_sub(lost);
                }
                aterm_log::warn!(
                    "quarantined corrupt warm block: {lost} lines lost \
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

    /// Handle memory pressure by evicting warm → cold, then cold FIFO.
    fn handle_memory_pressure(&mut self) {
        let mut changed = false;
        // First, evict warm blocks to cold (compresses further).
        // Stop when eviction fails: retrying the same block would loop forever.
        while self.over_budget() && self.warm.block_count() > 0 {
            if !self.evict_warm_to_cold() {
                break;
            }
            changed = true;
        }
        // Still over budget: batch-evict the oldest cold pages (`evict_bytes` is
        // O(P) total, not the O(K*P) of repeated pop_front).
        if self.over_budget() && self.cold.line_count() > 0 {
            let excess = self.budgeted_bytes.saturating_sub(self.memory_budget);
            let evicted = self.cold.evict_bytes(excess);
            self.line_count = self.line_count.saturating_sub(evicted.lines);
            // Real retention LOSS (not a user limit): count it so the host can
            // surface the truncation out-of-band (audit E10a). A quarantined
            // block's blank rows were counted when it was quarantined, so they
            // count once.
            let newly_lost = evicted.lines.saturating_sub(evicted.already_lost);
            self.pressure_evicted_lines += newly_lost as u64;
            self.sync_accounting();
            changed = true;
        }
        if changed {
            self.assert_bytes_used_invariant();
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl Scrollback {
    /// Inject a corrupted warm block at the front of the warm tier: it reports
    /// `line_count` lines but its compressed data is invalid, so decompression
    /// and warm→cold eviction fail. For quarantine tests and, in a debug build,
    /// the grid's dense-history conformance tests; never in a release build.
    #[doc(hidden)]
    pub fn inject_corrupted_warm_block(&mut self, line_count: usize) {
        self.warm.push_front_corrupt(line_count);
        self.line_count += line_count;
        self.sync_accounting();
    }
}
