// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tab stop operations: HT/CHT (CSI Ps I), CBT (CSI Ps Z), HTS (ESC H), TBC
//! (CSI Ps g) and the default every-8 reset.
//!
//! Tab motion keeps `pending_wrap`, as xterm's `TabToNextStop()` and
//! `TabToPrevStop()` never touch `do_wrap`. The flag is only set at the line's
//! last writable column, where no further tab stop exists.

use super::{Grid, row_u16};

impl Grid {
    /// Tab (move to next tab stop).
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col >= old(self.storage.cursor.col)
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
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

    /// Margin-aware tab (DECLRMM): inside the horizontal margins HT stops at the
    /// right margin; outside them it uses the screen edge, like CUF.
    #[inline]
    pub fn tab_margin(&mut self, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.tab();
            return;
        }
        let margins = self.storage.horizontal_margins();
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        let col = self.storage.cursor.col;
        let in_margins = col >= margins.left && col <= margins.right;
        let right_bound = if in_margins {
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
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col >= old(self.storage.cursor.col)
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    #[inline]
    #[cfg(test)]
    pub fn tab_n(&mut self, n: u16) {
        for _ in 0..n {
            self.tab();
        }
    }

    /// Margin-aware tab forward by n stops (DECLRMM).
    #[inline]
    pub fn tab_n_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        for _ in 0..n {
            self.tab_margin(left_right_margin_mode);
        }
    }

    /// Back tab (CBT, CSI Ps Z): move to the previous tab stop, or column 0.
    ///
    /// ENSURES: self.storage.cursor.col <= old(self.storage.cursor.col)
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

    /// Margin-aware back tab (CBT): the left margin binds only when DECLRMM and
    /// origin mode are both on. VT510's CBT stops at column one, which DECOM
    /// makes the left margin; xterm's `TabToPrevStop` and Ghostty's
    /// `horizontalTabBack` clamp to it only under origin mode, and xterm's left
    /// margin is 0 unless DECLRMM is on. Unlike CUB, CBT does not clamp
    /// otherwise. A cursor already left of the margin stays put (as in Ghostty;
    /// xterm drags it forward).
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
            // At or left of the margin: a backward tab never moves the cursor
            // forward (Ghostty's guard, not xterm's clamp).
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
    ///
    /// ENSURES: self.storage.cursor.col <= old(self.storage.cursor.col)
    #[inline]
    #[cfg(test)]
    pub fn back_tab_n(&mut self, n: u16) {
        for _ in 0..n {
            self.back_tab();
        }
    }

    /// Margin-aware back tab by n stops (see [`Grid::back_tab_margin`]).
    #[inline]
    pub fn back_tab_n_margin(&mut self, n: u16, left_right_margin_mode: bool, origin_mode: bool) {
        for _ in 0..n {
            self.back_tab_margin(left_right_margin_mode, origin_mode);
        }
    }

    /// Set a tab stop at the current cursor column (HTS - Horizontal Tab Set).
    ///
    /// ENSURES: self.storage.cursor.col < self.storage.tab_stops.len() implies self.storage.tab_stops[self.storage.cursor.col]
    #[inline]
    pub fn set_tab_stop(&mut self) {
        let col = self.storage.cursor.col;
        self.storage.set_tab_stop_at(col);
    }

    /// Clear the tab stop at the current cursor column (TBC 0).
    ///
    /// ENSURES: self.storage.cursor.col < self.storage.tab_stops.len() implies !self.storage.tab_stops[self.storage.cursor.col]
    #[inline]
    pub fn clear_tab_stop(&mut self) {
        let col = self.storage.cursor.col;
        self.storage.clear_tab_stop_at(col);
    }

    /// Clear all tab stops (TBC 3).
    ///
    /// ENSURES: self.storage.tab_stops.iter().all(|&s| !s)
    #[inline]
    pub fn clear_all_tab_stops(&mut self) {
        self.storage.clear_all_tab_stops();
    }

    /// Reset tab stops to default (every 8 columns).
    ///
    /// ENSURES: self.storage.tab_stops.len() == usize::from(self.storage.cols)
    #[inline]
    pub fn reset_tab_stops(&mut self) {
        let cols = self.storage.cols;
        self.storage.reset_tab_stops(cols);
    }
}
