// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tab stop management operations.
//!
//! Handles horizontal tab stop features:
//! - Tab forward (HT / CHT — CSI Ps I)
//! - Back tab (CBT — CSI Ps Z)
//! - Tab stop set (HTS — ESC H)
//! - Tab stop clear (TBC — CSI Ps g)
//! - Tab stop reset (default every-8 pattern)
//!
//! Forward tab motion (HT/CHT) PRESERVES the `pending_wrap` flag — xterm's
//! `TabToNextStop()` (tabs.c) only calls `set_cur_col` and never touches
//! `screen->do_wrap`, so a TAB issued while wrap is pending leaves the
//! cursor at the margin with the wrap still pending (the next printable
//! wraps). Since `pending_wrap` is only ever set at the line's last
//! writable column — where no further tab stop exists — preserving it
//! never strands the flag mid-line.

use super::{Grid, row_u16};

impl Grid {
    /// Tab (move to next tab stop).
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col >= old(self.storage.cursor.col)
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    #[inline]
    pub fn tab(&mut self) {
        // NOTE: pending_wrap is deliberately preserved (xterm TabToNextStop
        // never touches do_wrap; see module docs).
        // Find the next tab stop after the current column
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
            // No tab stop found, move to last column
            self.storage.cursor.col = max_col;
        }
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Margin-aware tab (move to next tab stop, respecting DECLRMM).
    ///
    /// Per VT510: when DECLRMM is active and the cursor is within the
    /// horizontal margin region, HT stops at the right margin instead of
    /// the last column. When the cursor is outside the margins, HT uses
    /// the screen edge as the boundary — matching CUF behavior (#7461).
    #[inline]
    pub fn tab_margin(&mut self, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.tab();
            return;
        }
        // NOTE: pending_wrap is deliberately preserved (xterm TabToNextStop
        // never touches do_wrap; see module docs).
        let margins = self.storage.horizontal_margins();
        let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
        let col = self.storage.cursor.col;
        // Only constrain to margins when cursor is inside the margin region.
        // When outside, use the screen edge (matching cursor_forward_margin).
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

    /// Tab forward by n stops.
    ///
    /// Implements CHT (Cursor Horizontal Tab) - CSI Ps I.
    /// Moves cursor forward through n tab stops.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col >= old(self.storage.cursor.col)
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    #[inline]
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

    /// Back tab (move to previous tab stop).
    ///
    /// Implements CBT (Cursor Backward Tabulation) - CSI Ps Z.
    /// Moves cursor to the previous tab stop, or column 0 if no prior tab stop exists.
    ///
    /// ENSURES: self.storage.cursor.col <= old(self.storage.cursor.col)
    #[inline]
    pub fn back_tab(&mut self) {
        self.storage.clear_pending_wrap();
        // Find the previous tab stop before the current column
        let max_col = usize::from(self.storage.max_col_for_row(self.storage.cursor.row));
        let current = usize::from(self.storage.cursor.col).min(max_col);
        if current == 0 {
            return; // Already at column 0
        }
        for col in (0..current).rev() {
            if self.storage.tab_stops[col] {
                self.storage.cursor.col = row_u16(col);
                return;
            }
        }
        // No tab stop found, move to column 0
        self.storage.cursor.col = 0;
    }

    /// Margin-aware back tab (DECLRMM).
    ///
    /// Per VT510: when DECLRMM is active and the cursor is within the
    /// horizontal margin region, CBT stops at the left margin instead
    /// of column 0. When the cursor is outside the margins, CBT uses
    /// column 0 as the boundary — matching CUB behavior (#7461).
    #[inline]
    pub fn back_tab_margin(&mut self, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.back_tab();
            return;
        }
        self.storage.clear_pending_wrap();
        let margins = self.storage.horizontal_margins();
        let max_col = usize::from(self.storage.max_col_for_row(self.storage.cursor.row));
        let current = usize::from(self.storage.cursor.col).min(max_col);
        // Only constrain to margins when cursor is inside the margin region.
        // When outside, use column 0 (matching cursor_backward_margin).
        let in_margins =
            current >= usize::from(margins.left) && current <= usize::from(margins.right);
        let left_bound = if in_margins {
            usize::from(margins.left)
        } else {
            0
        };
        if current <= left_bound {
            return; // Already at or left of boundary
        }
        for col in (left_bound..current).rev() {
            if self.storage.tab_stops[col] {
                self.storage.cursor.col = row_u16(col);
                return;
            }
        }
        // No tab stop found, move to boundary
        self.storage.cursor.col = row_u16(left_bound);
    }

    /// Back tab by n stops.
    ///
    /// Moves cursor backward through n tab stops.
    ///
    /// ENSURES: self.storage.cursor.col <= old(self.storage.cursor.col)
    #[inline]
    pub fn back_tab_n(&mut self, n: u16) {
        for _ in 0..n {
            self.back_tab();
        }
    }

    /// Margin-aware back tab by n stops (DECLRMM).
    #[inline]
    pub fn back_tab_n_margin(&mut self, n: u16, left_right_margin_mode: bool) {
        for _ in 0..n {
            self.back_tab_margin(left_right_margin_mode);
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

    /// Check if there is a tab stop at the given column.
    ///
    /// Returns `false` if the column is out of bounds.
    ///
    /// ENSURES: col >= self.storage.cols implies result == false
    #[cfg(any(test, kani, feature = "testing"))]
    #[inline]
    #[must_use]
    pub fn is_tab_stop(&self, col: u16) -> bool {
        self.storage.is_tab_stop(col)
    }
}
