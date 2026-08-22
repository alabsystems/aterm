// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Front-truncation for [`DiskColdTier`] — logical removal of oldest lines.

use super::DiskColdTier;
use crate::disk_format::len_u32_to_usize;

impl DiskColdTier {
    /// Logically remove the oldest `n` lines without decompression.
    ///
    /// Advances `front_offset` by `n` and drops any pages that become fully
    /// consumed. O(1) when no page boundary is crossed; O(pages_dropped) when
    /// pages are consumed. No decompression is performed.
    ///
    /// Consumed pages remain in the file but are removed from the in-memory
    /// index so they are no longer accessible.
    pub(crate) fn truncate_front_lines(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        debug_assert!(
            n <= self.line_count,
            "truncate_front_lines({n}) exceeds line_count({})",
            self.line_count
        );
        if n > self.line_count {
            aterm_log::warn!(
                "disk truncate_front_lines({n}) exceeds line_count({}), saturating",
                self.line_count
            );
        }

        self.front_offset += n;
        self.line_count = self.line_count.saturating_sub(n);

        // Drop fully consumed pages from the front of the index.
        // Count first, then advance the cursor once.
        let mut pages_dropped = 0;
        let mut offset_consumed = 0usize;
        for entry in self.live_index() {
            let page_lines = len_u32_to_usize(entry.line_count);
            if self.front_offset - offset_consumed >= page_lines {
                offset_consumed += page_lines;
                pages_dropped += 1;
            } else {
                break;
            }
        }
        if pages_dropped > 0 {
            self.front_offset = self.front_offset.saturating_sub(offset_consumed);

            // O(pages_dropped) amortized index maintenance: the shared
            // cursor and absolute base advance; no surviving entry of either
            // vector is drained or rebased (the old path here memmoved the
            // page index AND memmoved+rebased the cumulative index — O(total
            // pages) per drop, reached from the line-limit enforcement of
            // every push in a capped session). See drop_front_index_entries.
            self.drop_front_index_entries(pages_dropped);
            // Invalidate cache — live page indices shifted.
            self.clear_page_cache();

            // Refresh accounting only when state that feeds calculate_memory_used
            // changed. With pages_dropped == 0 every term is untouched (drains
            // don't run, cache is intact; front_offset/line_count are inline
            // fields covered by size_of::<Self>), so bytes_used is still exact
            // and a full cache walk per truncated line would be pure overhead.
            self.reset_bytes_used();
        }

        // Compact when dead space exceeds live data (>50% waste).
        // Amortized O(1): compaction rewrites O(live_bytes), but only fires
        // after accumulating dead_bytes > live_bytes, so each byte is rewritten
        // at most once per full rotation of the scrollback.
        if self.file.is_some() && !self.live_index().is_empty() && self.dead_bytes() > self.live_bytes()
        {
            // Compaction failure is non-fatal — file works fine with dead space.
            let _ = self.compact();
        }
    }

    // pre_validate_truncate_back and truncate_back_lines are in disk_memory.rs.
}
