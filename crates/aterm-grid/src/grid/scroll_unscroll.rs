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
    /// ENSURES: result <= old(scrollback_lines())
    pub fn unscroll_from_scrollback(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        self.storage.clear_pending_wrap();

        // Drain the lazy buffer so history is the ring (newest) over the store.
        self.drain_lazy_buffer();

        // History's NEWEST lines are the ring rows directly above the viewport,
        // then the lazy buffer, then the tiered store. Budgeting, reading and
        // removing only the store (to 2026-09-12) pasted lines a whole ring older
        // than the ones above the screen and cut them out of the middle of
        // history, and a ring-only history (every GUI session under its ring cap)
        // took the blank-scroll arm below and lost the bottom rows. Pull
        // newest-first from the ring, then the store, but the store only when no
        // lazy-staged lines sit between them (a store detached for reflow keeps
        // them staged): the `fill_viewport_deficit_from_history` rule.
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

        // Read all lines BEFORE any destructive operations. If any line
        // fails to decompress, abort to prevent permanent data loss (#4521).
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

        // Remove recovered lines from history (Kitty spec, #4248): ring first
        // (they are the newest), then the store for any remainder. Ring rows go
        // only AFTER the fill: dropping them rotates `ring_head` and shrinks
        // `rows`, and the viewport indices above were computed before that.
        // If store removal fails (decompression error), lines remain in
        // scrollback (duplicated with grid) — preferable to silent data loss
        // (#4638). Defensive: with try-read-first (#4521) this branch is
        // unreachable, since both traverse the same tiers from newest.
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
        // Every history row OLDER than the removed suffix keeps its line, but
        // its absolute key `oldest_absolute_row() + i` just shifted by `n`
        // (`scrollback_lines()` shrank while `absolute_row_counter` did not
        // move). No content_gen / damage / absolute-row-revision signal
        // distinguishes this wholesale renumbering from an ordinary append
        // batch, so bump the dedicated epoch: absolute-row-keyed history
        // caches (the terminal's incremental search-index refresh) must
        // REBUILD, never carry shifted keys forward. Bumped even when the
        // defensively-retained removal-failure branch above fired
        // (unreachable per #4638): a spurious rebuild is harmless; a missed
        // one is silently stale search results.
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        // SELECTION CUSTODY Phase 4: a Kitty unscroll renumbers history WHOLESALE
        // (see the `history_renumber_epoch` bump just above), so absolute row
        // numbers shift under every anchor and no band can describe the damage.
        // `All` is the honest answer.
        self.force_selection_invalidation();
        // Only the scroll region rows changed — mark them, not the full screen.
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .damage
            .mark_rows(top_u16, bottom_u16.saturating_add(1));
        n
    }
}
