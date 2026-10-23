// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Back-removal for [`WarmTier`], backing `remove_newest`.

use super::{WarmBlock, WarmTier};

impl WarmTier {
    /// Remove the newest `n` lines from the back of the warm tier, decompressing
    /// the boundary block before modifying state so errors leave the tier unchanged.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `n <= self.line_count`.
    // Skip: Trust does not yet relate `keep` to `lines.len()` or prove `sum`
    // overflow and `pop_front().expect`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_back_lines(&mut self, n: usize) -> Result<(), crate::ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        debug_assert!(
            n <= self.line_count,
            "truncate_back_lines({n}) exceeds line_count({})",
            self.line_count
        );

        let mut whole_blocks = 0;
        let mut remaining = n;
        for block in self.blocks.iter().rev() {
            let actual_idx = self
                .blocks
                .len()
                .saturating_sub(1)
                .saturating_sub(whole_blocks);
            let available = if actual_idx == 0 {
                block.line_count().saturating_sub(self.front_offset)
            } else {
                block.line_count()
            };
            if remaining >= available {
                remaining = remaining.saturating_sub(available);
                whole_blocks = whole_blocks.saturating_add(1);
            } else {
                break;
            }
        }
        let boundary_trim = remaining;

        let boundary_replacement = if boundary_trim > 0 {
            let boundary_idx = self
                .blocks
                .len()
                .saturating_sub(1)
                .saturating_sub(whole_blocks);
            let Some(bb) = self.blocks.get(boundary_idx) else {
                return Ok(());
            };
            let lines = bb.decompress()?;
            debug_assert!(
                lines.len() >= boundary_trim,
                "decompress returned {} lines but boundary_trim is {}",
                lines.len(),
                boundary_trim,
            );
            let keep = lines.len().saturating_sub(boundary_trim);
            if keep == 0 {
                None
            } else {
                Some(WarmBlock::from_lines(&lines[..keep]))
            }
        } else {
            None
        };

        for _ in 0..whole_blocks {
            self.blocks.pop_back();
        }

        if boundary_trim > 0 && !self.blocks.is_empty() {
            self.blocks.pop_back();
            if let Some(replacement) = boundary_replacement {
                self.blocks.push_back(replacement);
            }
        }

        if self.blocks.is_empty() {
            self.front_offset = 0;
        }

        // Drop front blocks fully consumed by front_offset.
        while let Some(front) = self.blocks.front() {
            if self.front_offset >= front.line_count() {
                let block = self
                    .blocks
                    .pop_front()
                    .expect("invariant: front block exists while trimming warm back-removal");
                self.front_offset = self.front_offset.saturating_sub(block.line_count());
            } else {
                break;
            }
        }
        if self.blocks.is_empty() {
            self.front_offset = 0;
        }

        let physical: usize = self.blocks.iter().map(|b| b.line_count()).sum();
        self.line_count = physical.saturating_sub(self.front_offset);
        self.budgeted_bytes = self.blocks.iter().map(|b| b.compressed_size()).sum();
        self.rebuild_cumulative();
        self.clear_cache();
        let base = std::mem::size_of::<Self>();
        self.bytes_used.set(
            base.saturating_add(self.budgeted_bytes)
                .saturating_add(self.index_bytes())
                .saturating_add(self.cached_bytes()),
        );

        Ok(())
    }
}
