// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-promotion helpers for [`DiskBackedScrollback`].

use super::*;

impl DiskBackedScrollback {
    /// Promote oldest hot lines to warm tier.
    pub(super) fn promote_hot_to_warm(&mut self) -> std::io::Result<()> {
        if self.hot.len() < self.block_size {
            return Ok(());
        }

        // Take block_size lines from front of hot tier.
        let lines = self.hot.take_front(self.block_size);
        if lines.is_empty() {
            return Ok(());
        }

        // Compress and add to warm tier.
        self.warm.push_block(&lines);

        // If warm tier is over limit, evict to cold.
        if self.warm.line_count() > self.warm_limit {
            self.evict_warm_to_cold()?;
        }

        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }

    /// Evict oldest warm block to cold tier.
    fn evict_warm_to_cold(&mut self) -> std::io::Result<()> {
        if let Some(block) = self.warm.pop_front() {
            match block.to_cold_compressed() {
                Ok((compressed, line_count)) => {
                    if let Err(error) = self.cold.push_compressed(&compressed, line_count) {
                        // Cold write failed — restore block to warm to prevent data loss.
                        self.warm.push_front(block);
                        self.sync_accounting();
                        self.assert_bytes_used_invariant();
                        return Err(error);
                    }
                }
                Err(error) => {
                    // Re-compression failed — restore block to warm to prevent data loss.
                    aterm_log::warn!(
                        "evict_warm_to_cold: re-compression failed ({error}), restoring block to warm tier"
                    );
                    self.warm.push_front(block);
                    self.sync_accounting();
                    self.assert_bytes_used_invariant();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("warm-to-cold re-compression failed: {error}"),
                    ));
                }
            }
            self.sync_accounting();
            self.assert_bytes_used_invariant();
        }
        Ok(())
    }

    /// Handle memory pressure by evicting warm to cold.
    pub(super) fn handle_memory_pressure(&mut self) -> std::io::Result<()> {
        let mut changed = false;
        while self.over_budget() && self.warm.block_count() > 0 {
            if let Err(e) = self.evict_warm_to_cold() {
                aterm_log::warn!("handle_memory_pressure: eviction failed ({e}), stopping");
                break;
            }
            changed = true;
        }
        if changed {
            self.assert_bytes_used_invariant();
        }
        Ok(())
    }
}
