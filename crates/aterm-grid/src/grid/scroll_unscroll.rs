// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty CSI + T unscroll implementation.
//!
//! Recovers content from the scrollback buffer by scrolling down and filling
//! the top rows with previously scrolled-off content.
//!
//! Extracted from `scroll.rs` to keep it under 500 lines.

use super::{Grid, row_u16};
use aterm_scrollback::ScrollbackStorage;

impl Grid {
    /// Unscroll from scrollback: scroll content down and fill top rows from scrollback.
    ///
    /// This is the Kitty CSI + T extension. Instead of scrolling down with blank lines,
    /// it recovers content from the scrollback buffer.
    ///
    /// Returns the number of lines actually unscrolled (may be less than requested
    /// if scrollback has fewer lines available).
    ///
    /// ## Behavior
    ///
    /// - On primary screen with scrollback: recovers lines from scrollback
    /// - On alternate screen (no scrollback): falls back to regular scroll_region_down
    /// - When scroll region is active: only unscrolls within region
    ///
    /// ## Kitty Protocol Reference
    ///
    /// `CSI n + T` - Scroll down n lines, filling new lines from scrollback instead of blanks.
    /// See: <https://sw.kovidgoyal.net/kitty/unscroll/>
    ///
    /// REQUIRES: self.storage.scroll_region.top <= self.storage.scroll_region.bottom
    /// ENSURES: result <= n
    /// ENSURES: result <= old(scrollback.line_count())
    pub fn unscroll_from_scrollback(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        self.storage.clear_pending_wrap();

        // Drain lazy buffer first so all lines are in tiered scrollback.
        // Unscroll reads the N newest lines and then removes them — this
        // is simpler and correct when all lines are in one storage.
        self.drain_lazy_buffer();

        let scrollback_available = self
            .storage
            .scrollback
            .as_ref()
            .map_or(0, ScrollbackStorage::line_count);
        if scrollback_available == 0 {
            self.scroll_region_down(n);
            return 0;
        }

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);
        let n = n.min(scrollback_available).min(bottom - top + 1);

        // Read all lines BEFORE any destructive operations. If any line
        // fails to decompress, abort to prevent permanent data loss (#4521).
        let lines = match self.try_read_scrollback_lines(n) {
            Some(lines) => lines,
            None => return 0,
        };

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
        for (i, line_opt) in lines.into_iter().enumerate() {
            let row_idx = top + i;
            if let Some(line) = line_opt {
                self.fill_row_from_line(row_u16(row_idx), &line, cols);
            } else if let Some(r) = self.row_mut(row_u16(row_idx)) {
                r.clear();
            }
        }

        // Remove recovered lines from scrollback (Kitty spec, #4248).
        // If removal fails (decompression error), lines remain in scrollback
        // (duplicated with grid) — preferable to silent data loss (#4638).
        //
        // Defensive: with tier-aware remove_newest (#4638) and try-read-first
        // (#4521), this error branch is unreachable — if try_read succeeds,
        // remove_newest will too (both traverse the same tiers from newest).
        // Retained as safety net against future architectural changes.
        if let Some(scrollback) = self.storage.scrollback.as_mut()
            && let Err(e) = scrollback.remove_newest(n)
        {
            aterm_log::warn!(
                "unscroll_from_scrollback: failed to remove {n} lines from scrollback: {e}"
            );
        }
        self.storage.content_scroll_delta = i32::MAX;
        // Only the scroll region rows changed — mark them, not the full screen.
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .damage
            .mark_rows(top_u16, bottom_u16.saturating_add(1));
        n
    }

    /// Try to read `n` lines from scrollback in reverse order.
    ///
    /// Returns `None` if any line fails to decompress (caller should abort).
    /// Returns `Some(vec![None; n])` if no scrollback is attached.
    fn try_read_scrollback_lines(
        &mut self,
        n: usize,
    ) -> Option<Vec<Option<aterm_scrollback::Line>>> {
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
            return Some(vec![None; n]);
        };
        let mut all_ok = true;
        let lines: Vec<_> = (0..n)
            .map(|i| {
                let rev_idx = n - 1 - i;
                match scrollback.get_line_rev(rev_idx) {
                    Ok(cow_opt) => cow_opt.map(std::borrow::Cow::into_owned),
                    Err(e) => {
                        aterm_log::warn!("unscroll: decompression failed at line {rev_idx}: {e}");
                        all_ok = false;
                        None
                    }
                }
            })
            .collect();
        all_ok.then_some(lines)
    }
}
