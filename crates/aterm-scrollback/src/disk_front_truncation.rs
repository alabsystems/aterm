// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Front-truncation for [`DiskColdTier`] — logical removal of oldest lines.

use super::DiskColdTier;
use crate::disk_format::len_u32_to_usize;

impl DiskColdTier {
    /// Logically remove the oldest `n` lines without decompression.
    ///
    /// Advances `front_offset` by `n` and drops fully consumed pages from the
    /// in-memory index (their bytes stay in the file until compaction). O(1) when
    /// no page boundary is crossed, else O(pages_dropped).
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

            // O(pages_dropped): the shared cursor and absolute base advance; no
            // surviving entry is moved or rebased.
            self.drop_front_index_entries(pages_dropped);
            // Invalidate cache — live page indices shifted.
            self.clear_page_cache();

            // Accounting changes only when pages drop: front_offset and line_count
            // are inline fields already counted by size_of::<Self>.
            self.reset_bytes_used();
        }

        // Compact when dead space exceeds live data. Amortized O(1): each byte is
        // rewritten at most once per full rotation of the scrollback.
        if self.file.is_some()
            && !self.live_index().is_empty()
            && self.dead_bytes() > self.live_bytes()
        {
            // Compaction failure is non-fatal — file works fine with dead space.
            let _ = self.compact();
        }
    }

    // truncate_back_lines is in disk_memory.rs.
}
