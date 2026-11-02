// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tab stop operations: HT/CHT (CSI Ps I), CBT (CSI Ps Z), HTS (ESC H), TBC
//! (CSI Ps g), and default every-8 reset.

use super::{Grid, row_u16};

impl Grid {
    /// Tab (move to next tab stop).
    #[inline]
    pub fn tab(&mut self) {
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        let start = usize::from(self.storage.cursor.col.saturating_add(1));
        let end = usize::from(max_col);
        let mut found = false;
        if start <= end {
            for col in start..=end {
                if self.storage.tab_stops[col] {
                    self.storage.cursor.col = row_u16(col);
                    found = true;
                    break;
                }
            }
        }
        if !found {
            self.storage.cursor.col = max_col;
        }
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Margin-aware tab (DECLRMM): stops at the right margin unless the cursor is already past
    /// it, then at the screen edge, like CUF.
    #[inline]
    pub fn tab_margin(&mut self, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.tab();
            return;
        }
        let margins = self.storage.horizontal_margins();
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        let col = self.storage.cursor.col;
        let right_bound = if col <= margins.right {
            margins.right.min(max_col)
        } else {
            max_col
        };
        let start = usize::from(col.saturating_add(1));
        let end = usize::from(right_bound);
        let mut found = false;
        if start <= end {
            for col in start..=end {
                if self.storage.tab_stops[col] {
                    self.storage.cursor.col = row_u16(col);
                    found = true;
                    break;
                }
            }
        }
        if !found {
            self.storage.cursor.col = right_bound;
        }
    }

    /// Tab forward by n stops (CHT, CSI Ps I).
    #[inline]
    #[cfg(test)]
    pub fn tab_n(&mut self, n: u16) {
        for _ in 0..n {
            let col = self.storage.cursor.col;
            self.tab();
            if self.storage.cursor.col == col {
                break;
            }
        }
    }

    /// Margin-aware tab forward by n stops (DECLRMM).
    ///
    /// A step's target is a function of the cursor column alone (its row, the margins and the
    /// stops stay put), so a step that leaves the column where it was leaves it there for every
    /// step after: the loop stops there, as xterm's CHT stops when `TabToNextStop` fails, and
    /// costs at most one step per column whatever `n` (up to 65535) asks.
    #[inline]
    pub fn tab_n_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        for _ in 0..n {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_tab_step();
            let col = self.storage.cursor.col;
            self.tab_margin(left_right_margin_mode);
            if self.storage.cursor.col == col {
                break;
            }
        }
    }

    /// Back tab (CBT, CSI Ps Z): move to the previous tab stop, or column 0.
    #[inline]
    pub fn back_tab(&mut self) {
        let max_col = usize::from(self.storage.max_col_for_row(self.storage.cursor.row));
        let current = usize::from(self.storage.cursor.col).min(max_col);
        if current == 0 {
            return;
        }
        for col in (0..current).rev() {
            if self.storage.tab_stops[col] {
                self.storage.cursor.col = row_u16(col);
                return;
            }
        }
        self.storage.cursor.col = 0;
    }

    /// Margin-aware back tab (CBT): clamps to the left margin when DECLRMM and origin mode are both active.
    #[inline]
    pub fn back_tab_margin(&mut self, left_right_margin_mode: bool, origin_mode: bool) {
        if !(left_right_margin_mode && origin_mode) {
            self.back_tab();
            return;
        }
        let margins = self.storage.horizontal_margins();
        let max_col = usize::from(self.storage.max_col_for_row(self.storage.cursor.row));
        let current = usize::from(self.storage.cursor.col).min(max_col);
        let left_bound = usize::from(margins.left);
        if current <= left_bound {
            return;
        }
        for col in (left_bound..current).rev() {
            if self.storage.tab_stops[col] {
                self.storage.cursor.col = row_u16(col);
                return;
            }
        }
        self.storage.cursor.col = row_u16(left_bound);
    }

    /// Back tab by n stops.
    #[inline]
    #[cfg(test)]
    pub fn back_tab_n(&mut self, n: u16) {
        for _ in 0..n {
            let col = self.storage.cursor.col;
            self.back_tab();
            if self.storage.cursor.col == col {
                break;
            }
        }
    }

    /// Margin-aware back tab by n stops (see [`Grid::back_tab_margin`]), stopping at the first
    /// step that leaves the column where it was, as [`Grid::tab_n_margin`] does.
    #[inline]
    pub fn back_tab_n_margin(&mut self, n: u16, left_right_margin_mode: bool, origin_mode: bool) {
        for _ in 0..n {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_tab_step();
            let col = self.storage.cursor.col;
            self.back_tab_margin(left_right_margin_mode, origin_mode);
            if self.storage.cursor.col == col {
                break;
            }
        }
    }

    /// Set a tab stop at the current cursor column (HTS - Horizontal Tab Set).
    #[inline]
    pub fn set_tab_stop(&mut self) {
        let col = self.storage.cursor.col;
        self.storage.set_tab_stop_at(col);
    }

    /// Clear the tab stop at the current cursor column (TBC 0).
    #[inline]
    pub fn clear_tab_stop(&mut self) {
        let col = self.storage.cursor.col;
        self.storage.clear_tab_stop_at(col);
    }

    /// Clear all tab stops (TBC 3).
    #[inline]
    pub fn clear_all_tab_stops(&mut self) {
        self.storage.clear_all_tab_stops();
    }

    /// Reset tab stops to default (every 8 columns).
    #[inline]
    pub fn reset_tab_stops(&mut self) {
        let cols = self.storage.cols;
        self.storage.reset_tab_stops(cols);
    }
}
