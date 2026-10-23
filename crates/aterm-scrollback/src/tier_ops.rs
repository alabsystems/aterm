// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-crossing operations on [`Scrollback`]: methods that read or write across the hot/warm/cold tiers (`lib.rs` holds the struct, constructors and read-only accessors).

use std::borrow::Cow;

use super::{
    DEFAULT_RED_PERCENT, DEFAULT_YELLOW_PERCENT, Line, Scrollback, ScrollbackError, WatermarkLevel,
    YELLOW_EXIT_PERCENT, threshold_bytes,
};

impl Scrollback {
    /// Get a line by index (0 = oldest).
    #[must_use = "line data is discarded if not consumed"]
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
    #[must_use = "line data is discarded if not consumed"]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        if rev_idx >= self.line_count {
            return Ok(None);
        }
        self.get_line(self.line_count.saturating_sub(1).saturating_sub(rev_idx))
    }

    /// Set the memory budget (bytes).
    pub fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        self.memory_budget = budget.max(1);
        self.yellow_threshold = threshold_bytes(DEFAULT_YELLOW_PERCENT, self.memory_budget);
        self.yellow_exit_threshold = threshold_bytes(YELLOW_EXIT_PERCENT, self.memory_budget);
        self.red_threshold = threshold_bytes(DEFAULT_RED_PERCENT, self.memory_budget);
        self.watermark_level = WatermarkLevel::Green;
        self.update_watermark_level();
        self.handle_memory_pressure();
        self.assert_bytes_used_invariant();
        if self.over_budget() && self.warm.block_count() > 0 {
            let over_bytes = self.budgeted_bytes.saturating_sub(self.memory_budget);
            return Err(ScrollbackError::EnforcementFailed { over_bytes });
        }
        Ok(())
    }

    /// Set the line limit (maximum total lines allowed).
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
    pub fn push_line(&mut self, line: Line) {
        if self.hot.len() >= self.hot_limit {
            self.promote_hot_to_warm();
        }

        self.hot.push(line);
        self.line_count = self.line_count.saturating_add(1);
        self.sync_accounting();

        if let Some(max) = self.line_limit
            && self.line_count > max
            && let Err(e) = self.truncate(max)
        {
            aterm_log::warn!(
                "push_line: truncation to limit {max} failed ({e}), limit temporarily unenforced"
            );
        }

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
    pub fn clear(&mut self) {
        self.hot.clear();
        self.warm.clear();
        self.cold.clear();
        self.line_count = 0;
        self.sync_accounting();
        self.assert_bytes_used_invariant();
    }

    /// Remove the `n` most recent lines from scrollback.
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
    pub fn truncate(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 {
            self.clear();
            return Ok(());
        }
        if n >= self.line_count {
            return Ok(());
        }

        let to_remove = self.line_count.saturating_sub(n);

        let cold_lines = self.cold.line_count();
        let cold_remove = to_remove.min(cold_lines);
        let after_cold = to_remove.saturating_sub(cold_remove);

        let warm_lines = self.warm.line_count();
        let warm_remove = after_cold.min(warm_lines);
        let hot_remove = after_cold.saturating_sub(warm_remove);

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

        self.image_rows_dropped_by_compression = self
            .image_rows_dropped_by_compression
            .saturating_add(crate::count_image_rows(&lines));

        self.warm.push_block(&lines);

        while self.warm.line_count() > self.warm_limit && self.evict_warm_to_cold() {}

        self.sync_accounting();
        self.assert_bytes_used_invariant();
    }

    /// Evict the oldest warm block to cold.
    fn evict_warm_to_cold(&mut self) -> bool {
        let Some(block) = self.warm.pop_front() else {
            return false;
        };
        let accepted = self.cold.push_block(&block);
        if accepted == 0 && block.line_count() > 0 {
            // decompress() already incremented the failure counter when it
            // failed inside cold.push_block → to_cold_compressed → decompress.
            if block.is_quarantined() {
                let lost = block.line_count();
                self.pressure_evicted_lines =
                    self.pressure_evicted_lines.saturating_add(lost as u64);
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
                return true;
            }
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
        while self.over_budget() && self.warm.block_count() > 0 {
            if !self.evict_warm_to_cold() {
                break;
            }
            changed = true;
        }
        if self.over_budget() && self.cold.line_count() > 0 {
            let excess = self.budgeted_bytes.saturating_sub(self.memory_budget);
            let evicted = self.cold.evict_bytes(excess);
            self.line_count = self.line_count.saturating_sub(evicted.lines);
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
    /// Inject a corrupted warm block at the front of the warm tier: it reports `line_count` lines but its compressed data is invalid, so decompression and warm→cold eviction fail.
    #[doc(hidden)]
    pub fn inject_corrupted_warm_block(&mut self, line_count: usize) {
        self.warm.push_front_corrupt(line_count);
        self.line_count += line_count;
        self.sync_accounting();
    }
}
