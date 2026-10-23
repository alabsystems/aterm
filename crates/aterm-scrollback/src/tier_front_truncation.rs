// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Front truncation for [`WarmTier`], backing `Scrollback::truncate`.

use super::WarmTier;

impl WarmTier {
    /// Materialize front_offset by trimming the first block in place.
    pub(super) fn materialize_front_offset(&mut self) {
        if self.front_offset == 0 || self.blocks.is_empty() {
            return;
        }

        let mut blocks_dropped: usize = 0;
        while let Some(front) = self.blocks.front() {
            if self.front_offset < front.line_count() {
                break;
            }

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
                let surviving = lines.get(self.front_offset..).unwrap_or(&[]);
                let old_size = self.blocks[0].compressed_size();
                let replacement = super::WarmBlock::from_lines(surviving);
                let new_size = replacement.compressed_size();
                self.blocks[0] = replacement;
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
    /// # Panics
    ///
    /// Debug-asserts `n <= self.line_count` in test builds; other builds warn
    /// and saturate.
    pub(crate) fn truncate_front_lines(&mut self, n: usize) -> Result<(), crate::ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        #[cfg(test)]
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            let mut msg = String::from("warm truncate_front_lines(");
            msg.push_str(&crate::error::dec_string(n));
            msg.push_str(") exceeds line_count(");
            msg.push_str(&crate::error::dec_string(self.line_count));
            msg.push_str("), saturating");
            crate::log_shim::warn_str(&msg);
        }

        self.front_offset = self.front_offset.saturating_add(n);
        self.line_count = self.line_count.saturating_sub(n);

        let mut blocks_dropped: usize = 0;
        while let Some(front) = self.blocks.front() {
            if self.front_offset >= front.line_count() {
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
            self.clear_cache();
        }

        Ok(())
    }
}
