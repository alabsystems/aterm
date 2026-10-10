// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid-local facade for the pin and generation tracking helpers. Runtime
//! builds use only `GenerationTracker`; `Pin` and `PinnedRange` are
//! test-only here.

// Grid impl for pin operations

use super::Grid;

impl Grid {
    /// Get the absolute row counter (monotonically increasing).
    #[must_use]
    #[inline]
    pub fn absolute_row_counter(&self) -> u64 {
        self.storage.absolute_row_counter
    }

    /// Continue another grid's absolute row numbering after a checkpoint
    /// restore, so an absolute row recorded before the handoff (a shell mark,
    /// a pin) names the same line after it. Only for a grid nothing has
    /// observed yet.
    ///
    /// Clamped: never below what this grid already holds, and never within
    /// `2^16` rows of `u64::MAX`, so a forged counter cannot overflow a scroll.
    pub fn continue_absolute_numbering(&mut self, counter: u64) {
        const CEILING: u64 = u64::MAX >> 16;
        let floor =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        self.storage.absolute_row_counter = counter.min(CEILING).max(floor);
    }

    /// EVERY RETAINED ROW KEEPS A KEY AT OR ABOVE 0: raise the counter to the
    /// retained total (`visible_rows + scrollback_lines()`) where a resize left
    /// it below — the floor [`Self::continue_absolute_numbering`] keeps for a
    /// restore, kept for every resize.
    ///
    /// The two maps of an absolute row agree only above that floor: a mark is
    /// RECORDED through [`Self::visible_to_absolute`] (`counter − visible + r`)
    /// and read back through [`Self::oldest_absolute_row`] (`counter − visible −
    /// scrollback`, saturating at 0) plus the history count. Below it the read
    /// map saturates and the write map does not, so every OSC 133 mark a shell
    /// records from then on reads back as a row above its prompt (`line`,
    /// `blocks`; tab #1, 2026-10-01), for good — output moves the counter and
    /// the history together. A rows-only resize no longer gets here (its bottom
    /// arms move the counter with their rows); a WIDTH rewrap can, since it
    /// changes how many rows the same lines take (synchronously, or at the
    /// re-attach of an offloaded one). Raising the counter moves the write map
    /// onto the read map — the keys every read already used — and is reported
    /// as the renumbering it is to the write side.
    pub(crate) fn cover_retained_rows(&mut self) {
        let retained =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        if self.storage.absolute_row_counter < retained {
            self.storage.absolute_row_counter = retained;
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
            self.invalidate_host_coordinates();
        }
    }

    /// Convert visible coordinates to absolute row number.
    /// Formula: `absolute_row_counter - visible_rows + visible_row`.
    #[must_use]
    pub fn visible_to_absolute(&self, visible_row: u16) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_add(u64::from(visible_row))
    }

    /// The absolute row number of the oldest retained line (history index 0).
    /// Rows below it have been evicted.
    ///
    /// Formula: `absolute_row_counter - visible_rows - scrollback_lines()`.
    #[must_use]
    pub fn oldest_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.scrollback_lines() as u64)
    }

    /// Epoch of row renumberings invisible to `(content_gen,
    /// absolute_row_revision)` (see `GridStorage::history_renumber_epoch`): a
    /// width reflow or a Kitty CSI +T unscroll. On an advance, absolute-row-keyed
    /// caches rebuild and cached absolute rows (search hits, find anchors) are
    /// recomputed before use.
    #[must_use]
    #[inline]
    pub fn history_renumber_epoch(&self) -> u64 {
        self.storage.history_renumber_epoch
    }

    /// The absolute row shown at the top of the viewport:
    /// `absolute_row_counter - visible_rows - display_offset`. The reference
    /// point for prompt-to-prompt navigation.
    #[must_use]
    pub fn top_visible_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.storage.display_offset as u64)
    }

    /// Absolute row of the top visible line at the live bottom (xterm
    /// `buffer.active.baseY`): `oldest_absolute_row() + scrollback_lines()`.
    /// `usize` so wasm maps it to a plain JS number.
    #[must_use]
    #[inline]
    pub fn base_y(&self) -> usize {
        self.oldest_absolute_row() as usize + self.scrollback_lines()
    }

    /// Absolute row of the top visible line for the current viewport:
    /// `base_y() - display_offset`. The search/link origin.
    #[must_use]
    #[inline]
    pub fn display_origin_absolute(&self) -> usize {
        self.base_y().saturating_sub(self.display_offset())
    }
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
