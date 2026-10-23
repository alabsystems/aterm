// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Absolute row numbering: the counter and its maps to visible and history rows.

use super::Grid;

impl Grid {
    /// Get the absolute row counter (monotonically increasing).
    #[must_use]
    #[inline]
    pub const fn absolute_row_counter(&self) -> u64 {
        self.storage.absolute_row_counter
    }

    /// Restores the absolute row counter from a checkpoint, clamped between retained rows and `u64::MAX >> 16`.
    pub fn continue_absolute_numbering(&mut self, counter: u64) {
        const CEILING: u64 = u64::MAX >> 16;
        let floor =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        self.renumber_rows(counter.min(CEILING).max(floor));
    }

    /// Raise the counter to the retained total (`visible_rows + scrollback_lines()`)
    /// where it is below, and bump `history_renumber_epoch`: the floor
    /// [`Self::continue_absolute_numbering`] keeps for a restore, kept for every
    /// rewrap and scrollback attach.
    pub(crate) fn cover_retained_rows(&mut self) {
        let retained =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        if self.storage.absolute_row_counter < retained {
            self.renumber_rows(retained);
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
            self.invalidate_host_coordinates();
        }
    }

    /// Convert visible coordinates to absolute row number.
    #[must_use]
    #[inline]
    pub const fn visible_to_absolute(&self, visible_row: u16) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(self.storage.visible_rows as u64)
            .saturating_add(visible_row as u64)
    }

    /// The absolute row number of the oldest retained line (history index 0).
    #[must_use]
    #[inline]
    pub fn oldest_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows))
            .saturating_sub(self.scrollback_lines() as u64)
    }

    /// Epoch of row renumberings (such as width reflow or unscroll) that invalidate cached absolute rows.
    #[must_use]
    #[inline]
    pub const fn history_renumber_epoch(&self) -> u64 {
        self.storage.history_renumber_epoch
    }

    /// The absolute row shown at the top of the viewport (`absolute_row_counter - visible_rows - display_offset`).
    #[must_use]
    #[inline]
    pub const fn top_visible_absolute_row(&self) -> u64 {
        self.storage
            .absolute_row_counter
            .saturating_sub(self.storage.visible_rows as u64)
            .saturating_sub(self.storage.display_offset as u64)
    }

    /// Absolute row of the top visible line at the live bottom (`oldest_absolute_row() + scrollback_lines()`).
    #[must_use]
    #[inline]
    pub fn base_y(&self) -> usize {
        self.oldest_absolute_row() as usize + self.scrollback_lines()
    }

    /// Absolute row of the top visible line for the current viewport (`base_y() - display_offset`).
    #[must_use]
    #[inline]
    pub fn display_origin_absolute(&self) -> usize {
        self.base_y().saturating_sub(self.display_offset())
    }
}

#[cfg(test)]
#[path = "pin_tests.rs"]
mod tests;
