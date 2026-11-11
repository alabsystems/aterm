// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cursor movement operations.

use super::Grid;
#[cfg(test)]
use super::clamp_u16;

impl Grid {
    /// Set cursor position (clamped to bounds): the program addressing the screen (CUP, HVP, VPA, a home).
    #[inline]
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        if self.storage.fill_debt.rows != 0
            && row.min(self.storage.visible_rows.saturating_sub(1)) != self.storage.cursor.row
        {
            self.drop_fill_debt();
        }
        self.set_cursor_following_content(row, col);
    }

    /// [`Self::set_cursor`] for a position that follows the content rather than addressing the screen.
    #[inline]
    pub fn set_cursor_following_content(&mut self, row: u16, col: u16) {
        self.storage.clear_pending_wrap();
        let row = row.min(self.storage.visible_rows.saturating_sub(1));
        let col = self.storage.clamp_col_for_row(row, col);
        self.storage.set_cursor_position(row, col);
        self.note_cursor_moved_up();
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Move cursor to position.
    #[cfg(test)]
    #[inline]
    pub fn move_cursor_to(&mut self, row: u16, col: u16) {
        self.set_cursor(row, col);
    }

    /// Move cursor by relative offset.
    #[cfg(test)]
    #[inline]
    pub fn move_cursor_by(&mut self, dr: i32, dc: i32) {
        let new_row = clamp_u16(i32::from(self.storage.cursor.row) + dr);
        let new_col = clamp_u16(i32::from(self.storage.cursor.col) + dc);
        self.set_cursor(new_row, new_col);
    }

    /// Move cursor up by `n` rows under xterm vertical margin rules: clamp at `scroll_region.top`
    /// when `cursor.row >= scroll_region.top`, and at row `0` when above `scroll_region.top`.
    #[doc(hidden)]
    #[inline]
    pub fn cursor_up(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        let region = self.storage.scroll_region;
        let at_or_below_top = self.storage.cursor.row >= region.top;
        let min_row = if at_or_below_top { region.top } else { 0 };
        self.storage.cursor.row = self.storage.cursor.row.saturating_sub(n).max(min_row);
        self.note_cursor_moved_up();
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        debug_assert!(
            !at_or_below_top || self.storage.cursor.row >= self.storage.scroll_region.top
        );
    }

    /// Move cursor down by `n` rows under xterm vertical margin rules: clamp at
    /// `scroll_region.bottom` when `cursor.row <= scroll_region.bottom`, and at `visible_rows - 1`
    /// when below `scroll_region.bottom`.
    #[doc(hidden)]
    #[inline]
    pub fn cursor_down(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        let region = self.storage.scroll_region;
        let at_or_above_bottom = self.storage.cursor.row <= region.bottom;
        let max_row = if at_or_above_bottom {
            region.bottom
        } else {
            self.storage.visible_rows.saturating_sub(1)
        };
        self.storage.cursor.row = self.storage.cursor.row.saturating_add(n).min(max_row);
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        debug_assert!(
            !at_or_above_bottom || self.storage.cursor.row <= self.storage.scroll_region.bottom
        );
    }

    /// Move cursor down by n rows, clamping to the screen edge (VPR, `CSI Ps e`).
    #[doc(hidden)]
    #[inline]
    pub fn line_position_relative(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        let max_row = self.storage.visible_rows.saturating_sub(1);
        self.storage.cursor.row = self.storage.cursor.row.saturating_add(n).min(max_row);
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Move cursor forward (right) by n columns.
    #[doc(hidden)]
    #[inline]
    pub fn cursor_forward(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        self.storage.cursor.col = self.storage.cursor.col.saturating_add(n).min(max_col);
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Margin-aware cursor forward (DECLRMM).
    ///
    /// Stops at the right margin, or at the screen edge for a cursor already past it.
    #[inline]
    pub fn cursor_forward_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.cursor_forward(n);
            return;
        }
        self.storage.clear_pending_wrap();
        let margins = self.storage.horizontal_margins();
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        // xterm `CursorForward`: the right margin binds unless the cursor is past it.
        let right_bound = if self.storage.cursor.col <= margins.right {
            margins.right.min(max_col)
        } else {
            max_col
        };
        self.storage.cursor.col = self.storage.cursor.col.saturating_add(n).min(right_bound);
    }

    /// Move cursor backward (left) by n columns.
    #[doc(hidden)]
    #[inline]
    pub fn cursor_backward(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        self.storage.cursor.col = self.storage.cursor.col.saturating_sub(n);
    }

    /// Margin-aware cursor backward (DECLRMM).
    ///
    /// Stops at the left margin, or at column 0 for a cursor already left of it.
    #[inline]
    pub fn cursor_backward_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.cursor_backward(n);
            return;
        }
        self.storage.clear_pending_wrap();
        let margins = self.storage.horizontal_margins();
        // xterm `CursorBack`: the left margin binds unless the cursor is left of it.
        let left_bound = if self.storage.cursor.col >= margins.left {
            margins.left
        } else {
            0
        };
        self.storage.cursor.col = self.storage.cursor.col.saturating_sub(n).max(left_bound);
    }

    /// Move cursor to column 0 (carriage return).
    #[doc(hidden)]
    #[inline]
    pub fn carriage_return(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.cursor.col = 0;
    }

    /// Margin-aware carriage return (DECLRMM).
    #[inline]
    pub fn carriage_return_margin(&mut self, left_right_margin_mode: bool) {
        self.storage.clear_pending_wrap();
        if left_right_margin_mode {
            let left = self.storage.horizontal_margins().left;
            if self.storage.cursor.col >= left {
                self.storage.cursor.col = left;
                return;
            }
        }
        self.storage.cursor.col = 0;
    }

    /// Move cursor down one row, scrolling if at bottom of scroll region.
    #[doc(hidden)]
    #[inline]
    pub fn line_feed(&mut self) {
        let bottom = self.storage.scroll_region.bottom;
        match self.storage.cursor.row.cmp(&bottom) {
            std::cmp::Ordering::Less => {
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
            }
            std::cmp::Ordering::Equal => {
                self.scroll_region_up(1);
                self.storage.clear_pending_wrap();
            }
            std::cmp::Ordering::Greater => {
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row < self.storage.visible_rows.saturating_sub(1) {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                }
            }
        }
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Move cursor down one row with DECLRMM horizontal margin awareness.
    #[inline]
    pub fn line_feed_margined(&mut self, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.line_feed();
            return;
        }
        let margins = self.storage.horizontal_margins();
        let is_margined = !margins.is_full(self.storage.cols);
        if !is_margined {
            self.line_feed();
            return;
        }

        let col = self.storage.cursor.col;
        let cursor_in_margins = col >= margins.left && col <= margins.right;
        let bottom = self.storage.scroll_region.bottom;

        match self.storage.cursor.row.cmp(&bottom) {
            std::cmp::Ordering::Less => {
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
            }
            std::cmp::Ordering::Equal => {
                // Rectangular scroll, only with the cursor inside the margins.
                if cursor_in_margins {
                    self.scroll_region_up_margined(1, margins.left, margins.right);
                }
                self.storage.clear_pending_wrap();
            }
            std::cmp::Ordering::Greater => {
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row < self.storage.visible_rows.saturating_sub(1) {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                }
            }
        }

        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Move cursor up one row, scrolling if at top of scroll region.
    #[inline]
    pub fn reverse_line_feed(&mut self) {
        let top = self.storage.scroll_region.top;
        match self.storage.cursor.row.cmp(&top) {
            std::cmp::Ordering::Greater => {
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
            }
            std::cmp::Ordering::Equal => {
                self.scroll_region_down(1);
                self.storage.clear_pending_wrap();
            }
            std::cmp::Ordering::Less => {
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row > 0 {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
                }
            }
        }
        self.note_cursor_moved_up();
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Move cursor up one row with DECLRMM horizontal margin awareness.
    #[inline]
    pub fn reverse_line_feed_margined(&mut self, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.reverse_line_feed();
            return;
        }
        let margins = self.storage.horizontal_margins();
        let is_margined = !margins.is_full(self.storage.cols);
        if !is_margined {
            self.reverse_line_feed();
            return;
        }

        let col = self.storage.cursor.col;
        let cursor_in_margins = col >= margins.left && col <= margins.right;
        let top = self.storage.scroll_region.top;

        match self.storage.cursor.row.cmp(&top) {
            std::cmp::Ordering::Greater => {
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
            }
            std::cmp::Ordering::Equal => {
                // Rectangular scroll, only with the cursor inside the margins.
                if cursor_in_margins {
                    self.scroll_region_down_margined(1, margins.left, margins.right);
                }
                self.storage.clear_pending_wrap();
            }
            std::cmp::Ordering::Less => {
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row > 0 {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
                }
            }
        }

        self.note_cursor_moved_up();
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Backspace (move left by 1).
    #[inline]
    #[cfg(test)]
    pub fn backspace(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.cursor.col = self.storage.cursor.col.saturating_sub(1);
    }

    /// Save cursor position (DECSC).
    #[inline]
    #[cfg(test)]
    pub fn save_cursor(&mut self) {
        self.storage.save_cursor();
        debug_assert!(self.storage.saved_cursor().valid);
    }

    /// Restore cursor position (DECRC).
    #[inline]
    #[cfg(test)]
    pub fn restore_cursor(&mut self) {
        let saved = self.storage.saved_cursor();
        if saved.valid {
            let row = saved
                .cursor
                .row
                .min(self.storage.visible_rows.saturating_sub(1));
            let col = self.storage.clamp_col_for_row(row, saved.cursor.col);
            self.storage
                .restore_saved_cursor(super::Cursor::new(row, col));
            if col < self.storage.max_col_for_row(row) {
                self.storage.clear_pending_wrap();
            }
        }
    }
}

#[cfg(test)]
#[path = "cursor_ops_tests.rs"]
mod tests;
