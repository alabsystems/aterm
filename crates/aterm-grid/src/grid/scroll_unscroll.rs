// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty CSI + T unscroll: scroll down and fill the top rows from scrollback.

use super::{Grid, row_u16};
use aterm_scrollback::ScrollbackStorage;

impl Grid {
    /// Unscroll (Kitty `CSI n + T`, <https://sw.kovidgoyal.net/kitty/unscroll/>):
    /// scroll content down, filling new top rows from scrollback instead of
    /// blanks. Only within the scroll region when one is active; falls back to
    /// `scroll_region_down` without scrollback (alternate screen).
    ///
    /// Returns the number of lines unscrolled, at most the lines available.
    ///
    /// REQUIRES: self.storage.scroll_region.top <= self.storage.scroll_region.bottom
    /// ENSURES: result <= n
    /// ENSURES: result <= old(scrollback_lines())
    pub fn unscroll_from_scrollback(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        self.storage.clear_pending_wrap();

        // Drain the lazy buffer so history is the ring (newest) over the store.
        self.drain_lazy_buffer();

        // The drain keeps lines staged during a reflow's detach window; staged
        // lines sit between the ring and the store, so the store counts only
        // when nothing is staged (the `fill_viewport_deficit_from_history` rule).
        let ring = self.storage.ring_buffer_scrollback();
        let available = if self.storage.lazy_buffer_lines() > 0 {
            ring
        } else {
            ring + self
                .storage
                .scrollback
                .as_ref()
                .map_or(0, ScrollbackStorage::line_count)
        };
        if available == 0 {
            self.scroll_region_down(n);
            return 0;
        }

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);
        let n = n.min(available).min(bottom - top + 1);

        // Read every line before mutating anything, so a decompression failure
        // loses nothing.
        let mut lines = Vec::with_capacity(n);
        for rev_idx in (0..n).rev() {
            match self.try_history_line_rev(rev_idx) {
                Ok(Some(line)) => lines.push(line.into_owned()),
                Ok(None) => return 0,
                Err(e) => {
                    aterm_log::warn!("unscroll: decompression failed at line {rev_idx}: {e}");
                    return 0;
                }
            }
        }

        // Row access during the unscroll writes must target the live viewport,
        // not a scrolled-back projection.
        self.reset_display_offset_with_damage();
        self.shift_rows_down(top, bottom, n);
        let (t, b) = (
            self.storage.scroll_region.top,
            self.storage.scroll_region.bottom,
        );
        self.storage.extras.shift_region_down_by(t, b, row_u16(n));

        let cols = self.storage.cols;
        for (i, line) in lines.iter().enumerate() {
            self.fill_row_from_line(row_u16(top + i), line, cols);
        }
        self.paint_wrap_holes(row_u16(top)..row_u16(top + lines.len()));

        // Remove the recovered lines, ring (newest) first. Ring rows go only
        // after the fill: dropping them rotates `ring_head`, which the indices
        // above predate. A failed store removal duplicates lines, never loses them.
        let from_ring = n.min(ring);
        if from_ring > 0 {
            self.drop_newest_ring_scrollback(from_ring);
        }
        let from_tiered = n - from_ring;
        if from_tiered > 0
            && let Some(scrollback) = self.storage.scrollback.as_mut()
            && let Err(e) = scrollback.remove_newest(from_tiered)
        {
            aterm_log::warn!(
                "unscroll_from_scrollback: failed to remove {from_tiered} lines from scrollback: {e}"
            );
        }
        // Older history rows keep their lines but their absolute keys shift by
        // `n`, which no content_gen or damage signal describes: bump the epoch so
        // absolute-row-keyed caches (the search index) rebuild, and drop the
        // selection band wholesale.
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        self.force_selection_invalidation();
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .damage
            .mark_rows(top_u16, bottom_u16.saturating_add(1));
        n
    }
}
