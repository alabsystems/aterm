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

    /// Move cursor up by n rows, respecting scroll region margins.
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

    /// Move cursor down by n rows, respecting scroll region margins.
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
mod tests {
    use crate::{Cursor, Grid};

    type Motion = fn(&mut Grid);

    /// One cursor motion from a start position under an optional DECSTBM region
    /// and DECLRMM margins, and where it must land.
    struct Case {
        name: &'static str,
        size: (u16, u16),
        region: Option<(u16, u16)>,
        margins: Option<(u16, u16)>,
        from: (u16, u16),
        wrap: bool,
        motion: Motion,
        want: (u16, u16),
    }

    const fn case(
        name: &'static str,
        size: (u16, u16),
        from: (u16, u16),
        motion: Motion,
        want: (u16, u16),
    ) -> Case {
        Case {
            name,
            size,
            region: None,
            margins: None,
            from,
            wrap: false,
            motion,
            want,
        }
    }

    const fn region(mut c: Case, top: u16, bottom: u16) -> Case {
        c.region = Some((top, bottom));
        c
    }

    const fn margins(mut c: Case, left: u16, right: u16) -> Case {
        c.margins = Some((left, right));
        c
    }

    const fn wrap(mut c: Case) -> Case {
        c.wrap = true;
        c
    }

    /// EVERY CURSOR MOTION LANDS WHERE XTERM PUTS IT: absolute moves clamp to
    /// the screen; CUU/CUD clamp on the margin they cross toward (even from
    /// outside the region), VPR on the screen edge; CUF/CUB and CR honour
    /// DECLRMM margins only in margin mode (CUF's right margin unless the
    /// cursor is past it, CUB's and CR's left margin unless it is left of it);
    /// LF/RI scroll at the region edge and move freely outside it; and each
    /// motion that consumes the pending wrap clears it.
    #[test]
    fn every_motion_lands_where_xterm_puts_it() {
        let cases = [
            wrap(case(
                "CUP",
                (24, 80),
                (0, 0),
                |g| g.set_cursor(12, 40),
                (12, 40),
            )),
            case(
                "CUP clamps row",
                (10, 80),
                (0, 0),
                |g| g.set_cursor(100, 5),
                (9, 5),
            ),
            case(
                "CUP clamps col",
                (10, 20),
                (0, 0),
                |g| g.set_cursor(3, 200),
                (3, 19),
            ),
            case(
                "CUP clamps both",
                (5, 10),
                (0, 0),
                |g| g.set_cursor(u16::MAX, u16::MAX),
                (4, 9),
            ),
            case(
                "move_cursor_to clamps",
                (5, 10),
                (0, 0),
                |g| g.move_cursor_to(50, 50),
                (4, 9),
            ),
            wrap(case("CUU", (24, 80), (10, 5), |g| g.cursor_up(3), (7, 5))),
            case("CUU 0", (24, 80), (10, 5), |g| g.cursor_up(0), (10, 5)),
            case(
                "CUU to row 0",
                (10, 80),
                (2, 5),
                |g| g.cursor_up(100),
                (0, 5),
            ),
            region(
                case(
                    "CUU in region",
                    (20, 80),
                    (10, 0),
                    |g| g.cursor_up(100),
                    (5, 0),
                ),
                5,
                15,
            ),
            region(
                case(
                    "CUU above region",
                    (20, 80),
                    (2, 0),
                    |g| g.cursor_up(100),
                    (0, 0),
                ),
                5,
                15,
            ),
            region(
                case(
                    "CUU below region",
                    (20, 80),
                    (15, 0),
                    |g| g.cursor_up(100),
                    (3, 0),
                ),
                3,
                12,
            ),
            wrap(case(
                "CUD",
                (24, 80),
                (5, 10),
                |g| g.cursor_down(3),
                (8, 10),
            )),
            case("CUD 0", (24, 80), (5, 10), |g| g.cursor_down(0), (5, 10)),
            case(
                "CUD to last row",
                (10, 80),
                (5, 0),
                |g| g.cursor_down(100),
                (9, 0),
            ),
            region(
                case(
                    "CUD in region",
                    (20, 80),
                    (8, 0),
                    |g| g.cursor_down(100),
                    (12, 0),
                ),
                3,
                12,
            ),
            region(
                case(
                    "CUD below region",
                    (20, 80),
                    (15, 0),
                    |g| g.cursor_down(100),
                    (19, 0),
                ),
                3,
                12,
            ),
            region(
                case(
                    "CUD above region",
                    (20, 80),
                    (1, 0),
                    |g| g.cursor_down(100),
                    (12, 0),
                ),
                3,
                12,
            ),
            region(
                case(
                    "VPR ignores the region",
                    (20, 80),
                    (5, 0),
                    |g| g.line_position_relative(100),
                    (19, 0),
                ),
                3,
                12,
            ),
            case("CUF", (10, 80), (0, 10), |g| g.cursor_forward(5), (0, 15)),
            case(
                "CUF to edge",
                (10, 20),
                (0, 15),
                |g| g.cursor_forward(100),
                (0, 19),
            ),
            wrap(case(
                "CUF 0",
                (10, 80),
                (0, 79),
                |g| g.cursor_forward(0),
                (0, 79),
            )),
            case("CUB", (10, 80), (0, 20), |g| g.cursor_backward(5), (0, 15)),
            case(
                "CUB to col 0",
                (10, 80),
                (0, 3),
                |g| g.cursor_backward(100),
                (0, 0),
            ),
            case(
                "CUB at col 0",
                (10, 80),
                (0, 0),
                |g| g.cursor_backward(5),
                (0, 0),
            ),
            wrap(case(
                "CUB wrapped",
                (10, 80),
                (0, 79),
                |g| g.cursor_backward(1),
                (0, 78),
            )),
            wrap(case("CR", (10, 80), (5, 40), Grid::carriage_return, (5, 0))),
            case(
                "CR no margin mode",
                (10, 80),
                (0, 40),
                |g| g.carriage_return_margin(false),
                (0, 0),
            ),
            margins(
                case(
                    "CR in margins",
                    (10, 80),
                    (0, 40),
                    |g| g.carriage_return_margin(true),
                    (0, 10),
                ),
                10,
                70,
            ),
            margins(
                case(
                    "CR left of margin",
                    (10, 80),
                    (0, 5),
                    |g| g.carriage_return_margin(true),
                    (0, 0),
                ),
                10,
                70,
            ),
            case(
                "CUF no margin mode",
                (10, 80),
                (0, 10),
                |g| g.cursor_forward_margin(5, false),
                (0, 15),
            ),
            margins(
                case(
                    "CUF in margins",
                    (10, 80),
                    (0, 30),
                    |g| g.cursor_forward_margin(100, true),
                    (0, 50),
                ),
                10,
                50,
            ),
            // xterm `CursorForward` lets go of the right margin only past it.
            margins(
                case(
                    "CUF from left of the margins",
                    (10, 80),
                    (0, 5),
                    |g| g.cursor_forward_margin(100, true),
                    (0, 50),
                ),
                10,
                50,
            ),
            margins(
                case(
                    "CUF from past the right margin",
                    (10, 80),
                    (0, 60),
                    |g| g.cursor_forward_margin(100, true),
                    (0, 79),
                ),
                10,
                50,
            ),
            case(
                "CUB no margin mode",
                (10, 80),
                (0, 20),
                |g| g.cursor_backward_margin(5, false),
                (0, 15),
            ),
            margins(
                case(
                    "CUB in margins",
                    (10, 80),
                    (0, 30),
                    |g| g.cursor_backward_margin(100, true),
                    (0, 10),
                ),
                10,
                50,
            ),
            margins(
                case(
                    "CUB from left of the margins",
                    (10, 80),
                    (0, 5),
                    |g| g.cursor_backward_margin(100, true),
                    (0, 0),
                ),
                10,
                50,
            ),
            // xterm `CursorBack` lets go of the left margin only left of it.
            margins(
                case(
                    "CUB from past the right margin",
                    (10, 80),
                    (0, 60),
                    |g| g.cursor_backward_margin(100, true),
                    (0, 10),
                ),
                10,
                50,
            ),
            case("LF", (24, 80), (5, 10), Grid::line_feed, (6, 10)),
            region(
                case(
                    "LF at region bottom scrolls",
                    (10, 80),
                    (7, 0),
                    Grid::line_feed,
                    (7, 0),
                ),
                2,
                7,
            ),
            region(
                case("LF below region", (10, 80), (7, 0), Grid::line_feed, (8, 0)),
                2,
                5,
            ),
            region(
                case(
                    "LF at last row below region",
                    (10, 80),
                    (9, 0),
                    Grid::line_feed,
                    (9, 0),
                ),
                2,
                5,
            ),
            wrap(case(
                "RI",
                (24, 80),
                (5, 10),
                Grid::reverse_line_feed,
                (4, 10),
            )),
            region(
                case(
                    "RI at region top scrolls",
                    (10, 80),
                    (3, 0),
                    Grid::reverse_line_feed,
                    (3, 0),
                ),
                3,
                7,
            ),
            region(
                case(
                    "RI above region",
                    (10, 80),
                    (2, 0),
                    Grid::reverse_line_feed,
                    (1, 0),
                ),
                5,
                8,
            ),
            region(
                case(
                    "RI at row 0 above region",
                    (10, 80),
                    (0, 0),
                    Grid::reverse_line_feed,
                    (0, 0),
                ),
                3,
                8,
            ),
        ];
        for c in cases {
            let mut grid = Grid::new(c.size.0, c.size.1);
            if let Some((top, bottom)) = c.region {
                grid.set_scroll_region(top, bottom);
            }
            if let Some((left, right)) = c.margins {
                grid.set_horizontal_margins(left, right);
            }
            grid.set_cursor(c.from.0, c.from.1);
            if c.wrap {
                grid.set_pending_wrap(true);
            }
            (c.motion)(&mut grid);
            assert_eq!(grid.cursor(), Cursor::new(c.want.0, c.want.1), "{}", c.name);
            assert_eq!(
                (grid.cursor_row(), grid.cursor_col()),
                c.want,
                "{}: accessors",
                c.name
            );
            if c.wrap {
                assert!(!grid.pending_wrap(), "{}: pending wrap survived", c.name);
            }
        }
    }

    /// One arm an index at a margin can take.
    #[derive(Clone, Copy, Debug)]
    struct MarginArm {
        /// DECLRMM.
        mode: bool,
        /// DECSLRM margins `(left, right)`, if set.
        margins: Option<(u16, u16)>,
        /// The cursor's column, a wrap pending there.
        col: u16,
        /// Whether the index scrolls the rectangle.
        scrolls: bool,
    }

    /// The four arms: DECLRMM off; on with the cursor on the right margin
    /// inside the rectangle (the margins reach the last column, or stop short
    /// of it); on with the cursor right of the margins, parked on the screen's
    /// last column.
    const MARGIN_ARMS: [MarginArm; 4] = [
        MarginArm {
            mode: false,
            margins: None,
            col: 9,
            scrolls: true,
        },
        MarginArm {
            mode: true,
            margins: Some((2, 9)),
            col: 9,
            scrolls: true,
        },
        MarginArm {
            mode: true,
            margins: Some((2, 5)),
            col: 5,
            scrolls: true,
        },
        MarginArm {
            mode: true,
            margins: Some((2, 5)),
            col: 9,
            scrolls: false,
        },
    ];

    /// A 4x10 grid for `arm`, with a marker `q` at (`row`, 3), inside every
    /// arm's rectangle, and a pending wrap at (`row`, `arm.col`).
    fn margin_arm_grid(row: u16, arm: MarginArm) -> Grid {
        let mut grid = Grid::new(4, 10);
        grid.set_cursor(row, 3);
        grid.write_char('q');
        if let Some((left, right)) = arm.margins {
            grid.set_horizontal_margins(left, right);
        }
        assert_eq!(!grid.horizontal_margins().is_full(grid.cols()), arm.mode);
        grid.set_cursor(row, arm.col);
        grid.set_pending_wrap(true);
        grid
    }

    /// At the top margin too: xterm's `RevIndex` scrolls, then `CursorUp(0)`
    /// resets the wrap (outside the margins it is `CursorUp(1)`, which the
    /// margin stops).
    #[test]
    fn reverse_line_feed_at_the_top_margin_clears_pending_wrap() {
        for arm in MARGIN_ARMS {
            let mut grid = margin_arm_grid(0, arm);
            grid.reverse_line_feed_margined(arm.mode);
            assert!(!grid.pending_wrap(), "{arm:?}: the wrap goes");
            assert_eq!(
                (grid.cursor_row(), grid.cursor_col()),
                (0, arm.col),
                "{arm:?}"
            );
            let marker_row = if arm.scrolls { 1 } else { 0 };
            assert_eq!(
                grid.cell(marker_row, 3).map(|c| c.char()),
                Some('q'),
                "{arm:?}: the rectangle scrolls only with the cursor inside it"
            );
        }
    }

    /// LF/IND at the bottom margin, the mirror image: xterm's `xtermIndex` is
    /// `CursorDown` (by 0 to the margin, then `xtermScroll`; by 1 outside the
    /// margins, which the margin stops), and `CursorDown` resets the wrap.
    #[test]
    fn line_feed_at_the_bottom_margin_clears_pending_wrap() {
        for arm in MARGIN_ARMS {
            let mut grid = margin_arm_grid(3, arm);
            grid.line_feed_margined(arm.mode);
            assert!(!grid.pending_wrap(), "{arm:?}: the wrap goes");
            assert_eq!(
                (grid.cursor_row(), grid.cursor_col()),
                (3, arm.col),
                "{arm:?}"
            );
            let marker_row = if arm.scrolls { 2 } else { 3 };
            assert_eq!(
                grid.cell(marker_row, 3).map(|c| c.char()),
                Some('q'),
                "{arm:?}: the rectangle scrolls only with the cursor inside it"
            );
        }
    }

    /// A one-row screen's region is that row, and a reverse index there
    /// scrolls it out (xterm `RevScroll` clamps to one line and clears it).
    #[test]
    fn reverse_line_feed_on_a_one_row_screen_blanks_the_row() {
        let mut grid = Grid::new(1, 10);
        grid.write_char('b');
        grid.set_cursor(0, 0);
        grid.reverse_line_feed();
        assert_eq!(grid.cell(0, 0).map(|c| c.char()), Some(' '));
    }

    /// The same under DECLRMM margins: an index either way on a one-row
    /// screen blanks the margin rectangle of its row and nothing outside it,
    /// as the full-width ops blank the whole row (the margined scrolls used to
    /// return early on `top == bottom` and leave the row standing).
    #[test]
    fn an_index_on_a_one_row_screen_blanks_the_margin_rectangle() {
        for reverse in [false, true] {
            let mut grid = Grid::new(1, 10);
            for ch in "abcdefgh".chars() {
                grid.write_char(ch);
            }
            grid.set_horizontal_margins(1, 5);
            grid.set_cursor(0, 3);
            if reverse {
                grid.reverse_line_feed_margined(true);
            } else {
                grid.line_feed_margined(true);
            }
            let text: String = (0..10)
                .map(|c| grid.cell(0, c).map_or(' ', |cell| cell.char()))
                .collect();
            assert_eq!(text, "a     gh  ", "reverse={reverse}");
            assert_eq!((grid.cursor_row(), grid.cursor_col()), (0, 3));
        }
    }

    #[test]
    fn grid_1x1_every_motion_stays_at_origin() {
        let motions: [(&str, Motion); 7] = [
            ("set_cursor(100, 100) clamps", |g| g.set_cursor(100, 100)),
            ("cursor_up(10)", |g| g.cursor_up(10)),
            ("cursor_down(10)", |g| g.cursor_down(10)),
            ("cursor_forward(10)", |g| g.cursor_forward(10)),
            ("cursor_backward(10)", |g| g.cursor_backward(10)),
            ("line_feed", Grid::line_feed),
            ("reverse_line_feed", Grid::reverse_line_feed),
        ];
        for (label, motion) in motions {
            let mut grid = Grid::new(1, 1);
            motion(&mut grid);
            assert_eq!(grid.cursor(), Cursor::new(0, 0), "{label}");
        }
    }
}
