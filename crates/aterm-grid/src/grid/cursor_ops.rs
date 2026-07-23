// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cursor movement operations.
//!
//! Tab stop management is in [`super::tab_ops`].
//!
//! All cursor movement methods clear the `pending_wrap` flag. This matches
//! xterm behavior where any cursor repositioning cancels the deferred wrap
//! state set by writing to the last column.

use super::Grid;
#[cfg(any(test, feature = "fuzz", fuzzing, feature = "testing"))]
use super::clamp_u16;

impl Grid {
    /// Set cursor position (clamped to bounds).
    ///
    /// Clears pending_wrap — explicit positioning cancels deferred wrap.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    #[inline]
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        self.storage.clear_pending_wrap();
        let row = row.min(self.storage.visible_rows.saturating_sub(1));
        let col = self.storage.clamp_col_for_row(row, col);
        self.storage.set_cursor_position(row, col);
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Move cursor to position.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    #[cfg(any(test, kani, feature = "testing"))]
    #[inline]
    pub fn move_cursor_to(&mut self, row: u16, col: u16) {
        self.set_cursor(row, col);
    }

    /// Move cursor by relative offset.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    #[cfg(any(test, feature = "fuzz", fuzzing, feature = "testing"))]
    #[inline]
    pub fn move_cursor_by(&mut self, dr: i32, dc: i32) {
        let new_row = clamp_u16(i32::from(self.storage.cursor.row) + dr);
        let new_col = clamp_u16(i32::from(self.storage.cursor.col) + dc);
        self.set_cursor(new_row, new_col);
    }

    /// Move cursor up by n rows, respecting scroll region margins.
    ///
    /// Per VT510: The cursor stops at the top margin if within the scroll region.
    /// If already above the top margin, stops at the top line (row 0).
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: old(cursor.row in scroll_region) implies self.storage.cursor.row >= self.storage.scroll_region.top
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn cursor_up(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        let region = self.storage.scroll_region;
        // xterm CursorUp: min = (cur_row < top_marg ? 0 : top_marg). The clamp
        // depends ONLY on the top margin — a cursor at or *below* the top margin
        // (including below the whole region) stops at the top margin, not just one
        // strictly inside the region. Requiring `row <= bottom` here let a cursor
        // below the region sail past the top margin to row 0.
        let at_or_below_top = self.storage.cursor.row >= region.top;
        let min_row = if at_or_below_top { region.top } else { 0 };
        self.storage.cursor.row = self.storage.cursor.row.saturating_sub(n).max(min_row);
        // Only clamp column when double-width rows exist. Row-only movement
        // cannot invalidate cursor.col when all rows share the same column count.
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        debug_assert!(
            !at_or_below_top || self.storage.cursor.row >= self.storage.scroll_region.top
        );
    }

    /// Move cursor down by n rows, respecting scroll region margins.
    ///
    /// Per VT510: The cursor stops at the bottom margin if within the scroll region.
    /// If already below the bottom margin, stops at the bottom line.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: old(cursor.row in scroll_region) implies self.storage.cursor.row <= self.storage.scroll_region.bottom
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn cursor_down(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        let region = self.storage.scroll_region;
        // xterm CursorDown: max = (cur_row > bot_marg ? max_row : bot_marg). The
        // clamp depends ONLY on the bottom margin — a cursor at or *above* the
        // bottom margin (including above the whole region) stops at the bottom
        // margin. Requiring `row >= top` here let a cursor above the region sail
        // past the bottom margin to the last line.
        let at_or_above_bottom = self.storage.cursor.row <= region.bottom;
        let max_row = if at_or_above_bottom {
            region.bottom
        } else {
            self.storage.visible_rows.saturating_sub(1)
        };
        self.storage.cursor.row = self.storage.cursor.row.saturating_add(n).min(max_row);
        // Only clamp column when double-width rows exist. Row-only movement
        // cannot invalidate cursor.col when all rows share the same column count.
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
    ///
    /// Unlike CUD, VPR is NOT bounded by the scroll region's bottom margin — it
    /// stops at the last visible line. xterm: VPR moves down within the page, not
    /// the region (matches the `cud-margin-clamp` conformance note).
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
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
    ///
    /// Stops at the right edge of the screen.
    ///
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    /// ENSURES: self.storage.cursor.col >= old(self.storage.cursor.col)
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
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
    /// Per VT510: when DECLRMM is active and the cursor is within the
    /// horizontal margins, CUF stops at the right margin. If the cursor
    /// is outside the margins, stops at the screen edge.
    #[inline]
    pub fn cursor_forward_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.cursor_forward(n);
            return;
        }
        self.storage.clear_pending_wrap();
        let margins = self.storage.horizontal_margins();
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        let in_margins =
            self.storage.cursor.col >= margins.left && self.storage.cursor.col <= margins.right;
        let right_bound = if in_margins {
            margins.right.min(max_col)
        } else {
            max_col
        };
        self.storage.cursor.col = self.storage.cursor.col.saturating_add(n).min(right_bound);
    }

    /// Move cursor backward (left) by n columns.
    ///
    /// Stops at the left edge of the screen (column 0).
    ///
    /// ENSURES: self.storage.cursor.col <= old(self.storage.cursor.col)
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn cursor_backward(&mut self, n: u16) {
        self.storage.clear_pending_wrap();
        self.storage.cursor.col = self.storage.cursor.col.saturating_sub(n);
    }

    /// Margin-aware cursor backward (DECLRMM).
    ///
    /// Per VT510: when DECLRMM is active and the cursor is within the
    /// horizontal margins, CUB stops at the left margin. If the cursor
    /// is outside the margins, stops at column 0.
    #[inline]
    pub fn cursor_backward_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.cursor_backward(n);
            return;
        }
        self.storage.clear_pending_wrap();
        let margins = self.storage.horizontal_margins();
        let in_margins =
            self.storage.cursor.col >= margins.left && self.storage.cursor.col <= margins.right;
        let left_bound = if in_margins { margins.left } else { 0 };
        self.storage.cursor.col = self.storage.cursor.col.saturating_sub(n).max(left_bound);
    }

    /// Move cursor to column 0 (carriage return).
    ///
    /// ENSURES: self.storage.cursor.col == 0
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn carriage_return(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.cursor.col = 0;
    }

    /// Margin-aware carriage return (DECLRMM).
    ///
    /// When `left_right_margin_mode` is true and the cursor is at or right of
    /// the left margin, the cursor moves to the left margin instead of column 0.
    /// This matches xterm's behavior for CR when DECLRMM (mode 69) is active.
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
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn line_feed(&mut self) {
        let bottom = self.storage.scroll_region.bottom;
        match self.storage.cursor.row.cmp(&bottom) {
            std::cmp::Ordering::Less => {
                // Within scroll region - just move down
                // (xterm CursorDown ends with ResetWrap)
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
            }
            std::cmp::Ordering::Equal => {
                // At bottom of scroll region - scroll within region. LF is a
                // cursor-down, so it ALSO resets the deferred wrap (xterm
                // CursorDown ResetWrap), even when it scrolls. scroll_region_up
                // preserves pending_wrap for the explicit SU/SD path, so clear it
                // here afterwards — otherwise the next glyph triggers a second
                // scroll (e.g. `\x1b[3;1Habcd\nZ` double-scrolled).
                self.scroll_region_up(1);
                self.storage.clear_pending_wrap();
            }
            std::cmp::Ordering::Greater => {
                // Below scroll region - move down if possible
                // (xterm CursorDown, clamped at max_row; ResetWrap either way)
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row < self.storage.visible_rows.saturating_sub(1) {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                }
            }
        }
        // Only clamp column when double-width rows exist. Row-only movement
        // cannot invalidate cursor.col when all rows share the same column count.
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Move cursor down one row with DECLRMM horizontal margin awareness (#7407).
    ///
    /// Per VT510: when DECLRMM is active and the cursor is within the horizontal
    /// margins, LF at the bottom of the scroll region scrolls only the cells
    /// within the margin region (rectangular scroll). When the cursor is outside
    /// the horizontal margins, LF at the scroll boundary does not trigger
    /// scrolling of the margin region — the cursor simply does not move.
    ///
    /// When `left_right_margin_mode` is false, behaves identically to `line_feed`.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
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
                // Within scroll region — move down regardless of margins
                // (xterm CursorDown ends with ResetWrap)
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
            }
            std::cmp::Ordering::Equal => {
                // At bottom of scroll region — scroll only if cursor is
                // within horizontal margins; use rectangular scroll.
                // pending_wrap is PRESERVED (xterm xtermScroll save/restore).
                if cursor_in_margins {
                    self.scroll_region_up_margined(1, margins.left, margins.right);
                }
                // If cursor is outside margins, no scroll and no cursor move.
            }
            std::cmp::Ordering::Greater => {
                // Below scroll region — move down if possible
                // (xterm CursorDown, clamped at max_row; ResetWrap either way)
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
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn reverse_line_feed(&mut self) {
        let top = self.storage.scroll_region.top;
        match self.storage.cursor.row.cmp(&top) {
            std::cmp::Ordering::Greater => {
                // Within scroll region - just move up
                // (xterm CursorUp ends with ResetWrap)
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
            }
            std::cmp::Ordering::Equal => {
                // At top of scroll region - scroll region down.
                // pending_wrap is PRESERVED: xterm RevScroll never touches
                // screen->do_wrap.
                self.scroll_region_down(1);
            }
            std::cmp::Ordering::Less => {
                // Above scroll region - move up if possible
                // (xterm CursorUp, clamped at row 0; ResetWrap either way)
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row > 0 {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
                }
            }
        }
        // Only clamp column when double-width rows exist. Row-only movement
        // cannot invalidate cursor.col when all rows share the same column count.
        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Move cursor up one row with DECLRMM horizontal margin awareness (#7407).
    ///
    /// Per VT510: when DECLRMM is active and the cursor is within the horizontal
    /// margins, RI at the top of the scroll region scrolls only the cells within
    /// the margin region down (rectangular scroll). When the cursor is outside
    /// the horizontal margins, RI at the scroll boundary does not trigger
    /// scrolling of the margin region — the cursor simply does not move.
    ///
    /// When `left_right_margin_mode` is false, behaves identically to
    /// `reverse_line_feed`.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
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
                // Within scroll region — move up regardless of margins
                // (xterm CursorUp ends with ResetWrap)
                self.storage.clear_pending_wrap();
                self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
            }
            std::cmp::Ordering::Equal => {
                // At top of scroll region — scroll only if cursor is
                // within horizontal margins; use rectangular scroll.
                // pending_wrap is PRESERVED (xterm RevScroll never touches it).
                if cursor_in_margins {
                    self.scroll_region_down_margined(1, margins.left, margins.right);
                }
                // If cursor is outside margins, no scroll and no cursor move.
            }
            std::cmp::Ordering::Less => {
                // Above scroll region — move up if possible
                // (xterm CursorUp, clamped at row 0; ResetWrap either way)
                self.storage.clear_pending_wrap();
                if self.storage.cursor.row > 0 {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_sub(1);
                }
            }
        }

        if self.storage.any_double_width {
            let row = self.storage.cursor.row;
            self.storage.cursor.col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Backspace (move left by 1).
    ///
    /// ENSURES: self.storage.cursor.col <= old(self.storage.cursor.col)
    /// ENSURES: old(self.storage.cursor.col) > 0 implies self.storage.cursor.col == old(self.storage.cursor.col) - 1
    #[inline]
    pub fn backspace(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.cursor.col = self.storage.cursor.col.saturating_sub(1);
    }

    /// Save cursor position (DECSC).
    ///
    /// Also saves the pending_wrap state, matching xterm DECSC behavior.
    ///
    /// ENSURES: self.storage.saved_cursor.valid == true
    /// ENSURES: self.storage.saved_cursor.cursor == old(self.storage.cursor)
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn save_cursor(&mut self) {
        self.storage.save_cursor();
        debug_assert!(self.storage.saved_cursor().valid);
    }

    /// Restore cursor position (DECRC).
    ///
    /// Also restores the pending_wrap state, matching xterm DECRC behavior.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.saved_cursor.valid implies self.storage.cursor.row < self.storage.visible_rows
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn restore_cursor(&mut self) {
        let saved = self.storage.saved_cursor();
        if saved.valid {
            // Restore position without clearing pending_wrap (set_cursor clears it)
            let row = saved
                .cursor
                .row
                .min(self.storage.visible_rows.saturating_sub(1));
            let col = self.storage.clamp_col_for_row(row, saved.cursor.col);
            self.storage
                .restore_saved_cursor(super::Cursor::new(row, col));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Cursor, Grid};

    // -------------------------------------------------------------------------
    // set_cursor — absolute positioning and clamping
    // -------------------------------------------------------------------------

    #[test]
    fn set_cursor_home_position() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.set_cursor(0, 0);
        assert_eq!(grid.cursor(), Cursor::new(0, 0));
    }

    #[test]
    fn set_cursor_exact_position() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(12, 40);
        assert_eq!(grid.cursor_row(), 12);
        assert_eq!(grid.cursor_col(), 40);
    }

    #[test]
    fn set_cursor_clamps_row_to_last_visible() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(100, 5);
        assert_eq!(grid.cursor_row(), 9);
        assert_eq!(grid.cursor_col(), 5);
    }

    #[test]
    fn set_cursor_clamps_col_to_max() {
        let mut grid = Grid::new(10, 20);
        grid.set_cursor(3, 200);
        assert_eq!(grid.cursor_row(), 3);
        assert_eq!(grid.cursor_col(), 19);
    }

    #[test]
    fn set_cursor_clamps_both_row_and_col() {
        let mut grid = Grid::new(5, 10);
        grid.set_cursor(u16::MAX, u16::MAX);
        assert_eq!(grid.cursor_row(), 4);
        assert_eq!(grid.cursor_col(), 9);
    }

    #[test]
    fn set_cursor_end_of_grid() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(23, 79);
        assert_eq!(grid.cursor(), Cursor::new(23, 79));
    }

    #[test]
    fn set_cursor_clears_pending_wrap() {
        let mut grid = Grid::new(24, 80);
        grid.set_pending_wrap(true);
        assert!(grid.pending_wrap());
        grid.set_cursor(5, 5);
        assert!(!grid.pending_wrap());
    }

    // -------------------------------------------------------------------------
    // move_cursor_to — alias for set_cursor
    // -------------------------------------------------------------------------

    #[test]
    fn move_cursor_to_absolute() {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(10, 20);
        assert_eq!(grid.cursor(), Cursor::new(10, 20));
    }

    #[test]
    fn move_cursor_to_clamps_out_of_bounds() {
        let mut grid = Grid::new(5, 10);
        grid.move_cursor_to(50, 50);
        assert_eq!(grid.cursor_row(), 4);
        assert_eq!(grid.cursor_col(), 9);
    }

    // -------------------------------------------------------------------------
    // move_cursor_by — relative movement
    // -------------------------------------------------------------------------

    #[test]
    fn move_cursor_by_positive_offset() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.move_cursor_by(3, 7);
        assert_eq!(grid.cursor(), Cursor::new(8, 17));
    }

    #[test]
    fn move_cursor_by_negative_offset() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(10, 20);
        grid.move_cursor_by(-5, -10);
        assert_eq!(grid.cursor(), Cursor::new(5, 10));
    }

    #[test]
    fn move_cursor_by_clamps_to_zero() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(3, 5);
        grid.move_cursor_by(-100, -100);
        assert_eq!(grid.cursor(), Cursor::new(0, 0));
    }

    #[test]
    fn move_cursor_by_clamps_to_max() {
        let mut grid = Grid::new(10, 20);
        grid.set_cursor(5, 10);
        grid.move_cursor_by(1000, 1000);
        assert_eq!(grid.cursor_row(), 9);
        assert_eq!(grid.cursor_col(), 19);
    }

    // -------------------------------------------------------------------------
    // cursor_up — vertical movement, scroll region interaction
    // -------------------------------------------------------------------------

    #[test]
    fn cursor_up_basic() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(10, 5);
        grid.cursor_up(3);
        assert_eq!(grid.cursor_row(), 7);
        assert_eq!(grid.cursor_col(), 5); // column unchanged
    }

    #[test]
    fn cursor_up_stops_at_row_zero_no_region() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(2, 5);
        grid.cursor_up(100);
        assert_eq!(grid.cursor_row(), 0);
    }

    #[test]
    fn cursor_up_within_scroll_region_stops_at_top_margin() {
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(5, 15);
        grid.set_cursor(10, 0);
        grid.cursor_up(100);
        assert_eq!(grid.cursor_row(), 5);
    }

    #[test]
    fn cursor_up_above_scroll_region_stops_at_zero() {
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(5, 15);
        grid.set_cursor(2, 0); // above region
        grid.cursor_up(100);
        assert_eq!(grid.cursor_row(), 0);
    }

    #[test]
    fn cursor_up_zero_does_nothing() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(10, 5);
        grid.cursor_up(0);
        assert_eq!(grid.cursor_row(), 10);
    }

    #[test]
    fn cursor_up_clears_pending_wrap() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 79);
        grid.set_pending_wrap(true);
        grid.cursor_up(1);
        assert!(!grid.pending_wrap());
        assert_eq!(grid.cursor_row(), 4);
    }

    // -------------------------------------------------------------------------
    // cursor_down — vertical movement, scroll region interaction
    // -------------------------------------------------------------------------

    #[test]
    fn cursor_down_basic() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.cursor_down(3);
        assert_eq!(grid.cursor_row(), 8);
        assert_eq!(grid.cursor_col(), 10);
    }

    #[test]
    fn cursor_down_stops_at_last_row_no_region() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(5, 0);
        grid.cursor_down(100);
        assert_eq!(grid.cursor_row(), 9);
    }

    #[test]
    fn cursor_down_within_scroll_region_stops_at_bottom_margin() {
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(3, 12);
        grid.set_cursor(8, 0);
        grid.cursor_down(100);
        assert_eq!(grid.cursor_row(), 12);
    }

    #[test]
    fn cursor_down_below_scroll_region_stops_at_last_line() {
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(3, 12);
        grid.set_cursor(15, 0); // below region
        grid.cursor_down(100);
        assert_eq!(grid.cursor_row(), 19);
    }

    #[test]
    fn cursor_down_above_scroll_region_stops_at_bottom_margin() {
        // xterm CursorDown clamps on the bottom margin alone: a cursor ABOVE the
        // region moving down still stops at the bottom margin, not the last line.
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(3, 12);
        grid.set_cursor(1, 0); // above region
        grid.cursor_down(100);
        assert_eq!(grid.cursor_row(), 12);
    }

    #[test]
    fn cursor_up_below_scroll_region_stops_at_top_margin() {
        // Symmetric: a cursor BELOW the region moving up stops at the top margin.
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(3, 12);
        grid.set_cursor(15, 0); // below region
        grid.cursor_up(100);
        assert_eq!(grid.cursor_row(), 3);
    }

    #[test]
    fn line_position_relative_ignores_bottom_margin() {
        // VPR (CSI Ps e) is page-relative: it clamps to the screen edge, not the
        // scroll region's bottom margin (unlike CUD).
        let mut grid = Grid::new(20, 80);
        grid.set_scroll_region(3, 12);
        grid.set_cursor(5, 0);
        grid.line_position_relative(100);
        assert_eq!(grid.cursor_row(), 19);
    }

    #[test]
    fn cursor_down_zero_does_nothing() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.cursor_down(0);
        assert_eq!(grid.cursor_row(), 5);
    }

    #[test]
    fn cursor_down_clears_pending_wrap() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 79);
        grid.set_pending_wrap(true);
        grid.cursor_down(1);
        assert!(!grid.pending_wrap());
        assert_eq!(grid.cursor_row(), 6);
    }

    // -------------------------------------------------------------------------
    // cursor_forward — horizontal movement right
    // -------------------------------------------------------------------------

    #[test]
    fn cursor_forward_basic() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 10);
        grid.cursor_forward(5);
        assert_eq!(grid.cursor_col(), 15);
    }

    #[test]
    fn cursor_forward_stops_at_right_edge() {
        let mut grid = Grid::new(10, 20);
        grid.set_cursor(0, 15);
        grid.cursor_forward(100);
        assert_eq!(grid.cursor_col(), 19);
    }

    #[test]
    fn cursor_forward_zero_does_nothing() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 10);
        grid.cursor_forward(0);
        assert_eq!(grid.cursor_col(), 10);
    }

    #[test]
    fn cursor_forward_clears_pending_wrap() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 79);
        grid.set_pending_wrap(true);
        grid.cursor_forward(0);
        assert!(!grid.pending_wrap());
    }

    // -------------------------------------------------------------------------
    // cursor_backward — horizontal movement left
    // -------------------------------------------------------------------------

    #[test]
    fn cursor_backward_basic() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 20);
        grid.cursor_backward(5);
        assert_eq!(grid.cursor_col(), 15);
    }

    #[test]
    fn cursor_backward_stops_at_column_zero() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 3);
        grid.cursor_backward(100);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn cursor_backward_from_zero() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 0);
        grid.cursor_backward(5);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn cursor_backward_clears_pending_wrap() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 79);
        grid.set_pending_wrap(true);
        grid.cursor_backward(1);
        assert!(!grid.pending_wrap());
        assert_eq!(grid.cursor_col(), 78);
    }

    // -------------------------------------------------------------------------
    // carriage_return
    // -------------------------------------------------------------------------

    #[test]
    fn carriage_return_moves_to_col_zero() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(5, 40);
        grid.carriage_return();
        assert_eq!(grid.cursor_col(), 0);
        assert_eq!(grid.cursor_row(), 5); // row unchanged
    }

    #[test]
    fn carriage_return_at_col_zero_is_noop() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(3, 0);
        grid.carriage_return();
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn carriage_return_clears_pending_wrap() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 79);
        grid.set_pending_wrap(true);
        grid.carriage_return();
        assert!(!grid.pending_wrap());
        assert_eq!(grid.cursor_col(), 0);
    }

    // -------------------------------------------------------------------------
    // carriage_return_margin — DECLRMM interaction
    // -------------------------------------------------------------------------

    #[test]
    fn carriage_return_margin_no_mode_goes_to_zero() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 40);
        grid.carriage_return_margin(false);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn carriage_return_margin_mode_cursor_in_margins() {
        let mut grid = Grid::new(10, 80);
        grid.set_horizontal_margins(10, 70);
        grid.set_cursor(0, 40); // within margins [10..70]
        grid.carriage_return_margin(true);
        assert_eq!(grid.cursor_col(), 10);
    }

    #[test]
    fn carriage_return_margin_mode_cursor_left_of_margin() {
        let mut grid = Grid::new(10, 80);
        grid.set_horizontal_margins(10, 70);
        grid.set_cursor(0, 5); // left of left margin
        grid.carriage_return_margin(true);
        assert_eq!(grid.cursor_col(), 0);
    }

    // -------------------------------------------------------------------------
    // line_feed — scroll region interaction
    // -------------------------------------------------------------------------

    #[test]
    fn line_feed_moves_down_within_region() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.line_feed();
        assert_eq!(grid.cursor_row(), 6);
        assert_eq!(grid.cursor_col(), 10);
    }

    #[test]
    fn line_feed_at_bottom_of_region_scrolls() {
        let mut grid = Grid::new(10, 80);
        grid.set_scroll_region(2, 7);
        grid.set_cursor(7, 0); // at bottom of region
        grid.line_feed();
        // After scroll, cursor stays at row 7 (bottom of region)
        assert_eq!(grid.cursor_row(), 7);
    }

    #[test]
    fn line_feed_below_region_moves_down() {
        let mut grid = Grid::new(10, 80);
        grid.set_scroll_region(2, 5);
        grid.set_cursor(7, 0); // below region
        grid.line_feed();
        assert_eq!(grid.cursor_row(), 8);
    }

    #[test]
    fn line_feed_at_last_row_below_region_stays() {
        let mut grid = Grid::new(10, 80);
        grid.set_scroll_region(2, 5);
        grid.set_cursor(9, 0); // last row, below region
        grid.line_feed();
        assert_eq!(grid.cursor_row(), 9);
    }

    #[test]
    fn line_feed_clears_pending_wrap() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 79);
        grid.set_pending_wrap(true);
        grid.line_feed();
        assert!(!grid.pending_wrap());
    }

    // -------------------------------------------------------------------------
    // reverse_line_feed
    // -------------------------------------------------------------------------

    #[test]
    fn reverse_line_feed_moves_up_within_region() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.reverse_line_feed();
        assert_eq!(grid.cursor_row(), 4);
        assert_eq!(grid.cursor_col(), 10);
    }

    #[test]
    fn reverse_line_feed_at_top_of_region_scrolls_down() {
        let mut grid = Grid::new(10, 80);
        grid.set_scroll_region(3, 7);
        grid.set_cursor(3, 0); // at top of region
        grid.reverse_line_feed();
        // After scroll-down, cursor stays at row 3
        assert_eq!(grid.cursor_row(), 3);
    }

    #[test]
    fn reverse_line_feed_above_region_moves_up() {
        let mut grid = Grid::new(10, 80);
        grid.set_scroll_region(5, 8);
        grid.set_cursor(2, 0); // above region
        grid.reverse_line_feed();
        assert_eq!(grid.cursor_row(), 1);
    }

    #[test]
    fn reverse_line_feed_at_row_zero_above_region_stays() {
        let mut grid = Grid::new(10, 80);
        grid.set_scroll_region(3, 8);
        grid.set_cursor(0, 0);
        grid.reverse_line_feed();
        assert_eq!(grid.cursor_row(), 0);
    }

    #[test]
    fn reverse_line_feed_clears_pending_wrap() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 79);
        grid.set_pending_wrap(true);
        grid.reverse_line_feed();
        assert!(!grid.pending_wrap());
    }

    // -------------------------------------------------------------------------
    // backspace
    // -------------------------------------------------------------------------

    #[test]
    fn backspace_moves_left_one() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 10);
        grid.backspace();
        assert_eq!(grid.cursor_col(), 9);
    }

    #[test]
    fn backspace_at_col_zero_stays() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(3, 0);
        grid.backspace();
        assert_eq!(grid.cursor_col(), 0);
        assert_eq!(grid.cursor_row(), 3); // row unchanged
    }

    #[test]
    fn backspace_clears_pending_wrap() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 79);
        grid.set_pending_wrap(true);
        grid.backspace();
        assert!(!grid.pending_wrap());
        assert_eq!(grid.cursor_col(), 78);
    }

    // -------------------------------------------------------------------------
    // save_cursor / restore_cursor (DECSC / DECRC)
    // -------------------------------------------------------------------------

    #[test]
    fn save_and_restore_cursor_roundtrip() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(10, 30);
        grid.save_cursor();

        grid.set_cursor(0, 0);
        assert_eq!(grid.cursor(), Cursor::new(0, 0));

        grid.restore_cursor();
        assert_eq!(grid.cursor(), Cursor::new(10, 30));
    }

    #[test]
    fn restore_cursor_without_save_is_noop() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(5, 10);
        grid.restore_cursor(); // no prior save — should not change cursor
        assert_eq!(grid.cursor(), Cursor::new(5, 10));
    }

    #[test]
    fn save_cursor_preserves_pending_wrap() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(0, 79);
        grid.set_pending_wrap(true);
        grid.save_cursor();

        // Move and clear wrap
        grid.set_cursor(5, 5);
        assert!(!grid.pending_wrap());

        grid.restore_cursor();
        assert!(grid.pending_wrap());
        assert_eq!(grid.cursor(), Cursor::new(0, 79));
    }

    #[test]
    fn restore_cursor_clamps_to_resized_grid() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(20, 70);
        grid.save_cursor();

        grid.resize(10, 40);
        grid.restore_cursor();
        assert_eq!(grid.cursor_row(), 9); // clamped to new rows - 1
        assert_eq!(grid.cursor_col(), 39); // clamped to new cols - 1
    }

    // -------------------------------------------------------------------------
    // cursor_forward_margin — DECLRMM interaction
    // -------------------------------------------------------------------------

    #[test]
    fn cursor_forward_margin_no_mode_same_as_forward() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 10);
        grid.cursor_forward_margin(5, false);
        assert_eq!(grid.cursor_col(), 15);
    }

    #[test]
    fn cursor_forward_margin_in_margins_stops_at_right() {
        let mut grid = Grid::new(10, 80);
        grid.set_horizontal_margins(10, 50);
        grid.set_cursor(0, 30); // within [10..50]
        grid.cursor_forward_margin(100, true);
        assert_eq!(grid.cursor_col(), 50);
    }

    #[test]
    fn cursor_forward_margin_outside_margins_goes_to_edge() {
        let mut grid = Grid::new(10, 80);
        grid.set_horizontal_margins(10, 50);
        grid.set_cursor(0, 5); // left of margins
        grid.cursor_forward_margin(100, true);
        assert_eq!(grid.cursor_col(), 79);
    }

    // -------------------------------------------------------------------------
    // cursor_backward_margin — DECLRMM interaction
    // -------------------------------------------------------------------------

    #[test]
    fn cursor_backward_margin_no_mode_same_as_backward() {
        let mut grid = Grid::new(10, 80);
        grid.set_cursor(0, 20);
        grid.cursor_backward_margin(5, false);
        assert_eq!(grid.cursor_col(), 15);
    }

    #[test]
    fn cursor_backward_margin_in_margins_stops_at_left() {
        let mut grid = Grid::new(10, 80);
        grid.set_horizontal_margins(10, 50);
        grid.set_cursor(0, 30); // within [10..50]
        grid.cursor_backward_margin(100, true);
        assert_eq!(grid.cursor_col(), 10);
    }

    #[test]
    fn cursor_backward_margin_outside_margins_goes_to_zero() {
        let mut grid = Grid::new(10, 80);
        grid.set_horizontal_margins(10, 50);
        grid.set_cursor(0, 5); // left of margins
        grid.cursor_backward_margin(100, true);
        assert_eq!(grid.cursor_col(), 0);
    }

    // -------------------------------------------------------------------------
    // Edge cases: 1x1 grid
    // -------------------------------------------------------------------------

    #[test]
    fn grid_1x1_set_cursor_clamped() {
        let mut grid = Grid::new(1, 1);
        grid.set_cursor(100, 100);
        assert_eq!(grid.cursor(), Cursor::new(0, 0));
    }

    #[test]
    fn grid_1x1_cursor_up_stays() {
        let mut grid = Grid::new(1, 1);
        grid.cursor_up(10);
        assert_eq!(grid.cursor_row(), 0);
    }

    #[test]
    fn grid_1x1_cursor_down_stays() {
        let mut grid = Grid::new(1, 1);
        grid.cursor_down(10);
        assert_eq!(grid.cursor_row(), 0);
    }

    #[test]
    fn grid_1x1_cursor_forward_stays() {
        let mut grid = Grid::new(1, 1);
        grid.cursor_forward(10);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn grid_1x1_cursor_backward_stays() {
        let mut grid = Grid::new(1, 1);
        grid.cursor_backward(10);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn grid_1x1_line_feed_stays() {
        let mut grid = Grid::new(1, 1);
        grid.line_feed();
        assert_eq!(grid.cursor_row(), 0);
    }

    #[test]
    fn grid_1x1_reverse_line_feed_stays() {
        let mut grid = Grid::new(1, 1);
        grid.reverse_line_feed();
        assert_eq!(grid.cursor_row(), 0);
    }

    // -------------------------------------------------------------------------
    // cursor_row / cursor_col accessor correctness after moves
    // -------------------------------------------------------------------------

    #[test]
    fn accessors_consistent_with_cursor_struct() {
        let mut grid = Grid::new(24, 80);
        grid.set_cursor(15, 42);
        let c = grid.cursor();
        assert_eq!(c.row, grid.cursor_row());
        assert_eq!(c.col, grid.cursor_col());
    }

    #[test]
    fn accessors_after_sequential_moves() {
        let mut grid = Grid::new(20, 60);
        grid.set_cursor(10, 30);
        grid.cursor_up(3);
        assert_eq!(grid.cursor_row(), 7);
        grid.cursor_forward(10);
        assert_eq!(grid.cursor_col(), 40);
        grid.cursor_down(5);
        assert_eq!(grid.cursor_row(), 12);
        grid.cursor_backward(20);
        assert_eq!(grid.cursor_col(), 20);
        grid.backspace();
        assert_eq!(grid.cursor_col(), 19);
        grid.carriage_return();
        assert_eq!(grid.cursor_col(), 0);
    }
}
