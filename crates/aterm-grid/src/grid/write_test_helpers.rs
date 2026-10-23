// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Test-only combined write and advance methods.

use super::{CellFlags, Grid, PackedColor};

impl Grid {
    /// Write a styled character at the cursor and advance it.
    pub(crate) fn write_char_styled(
        &mut self,
        c: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char_styled(cursor_col, c, fg, bg, flags);
        }
        self.storage.damage.mark_cell(cursor_row, cursor_col);

        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        }
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Write a styled character with autowrap.
    pub(crate) fn write_char_wrap_styled(
        &mut self,
        c: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) {
        self.resolve_pending_wrap();

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char_styled(cursor_col, c, fg, bg, flags);
        }
        self.storage.damage.mark_cell(cursor_row, cursor_col);

        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        } else {
            self.storage.mark_pending_wrap();
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Write a wide character with deferred autowrap.
    pub(crate) fn write_wide_char_wrap_styled(
        &mut self,
        c: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) -> bool {
        self.resolve_pending_wrap();

        let effective_cols = self.storage.effective_cols_for_row(self.storage.cursor.row);
        if self.storage.cursor.col.saturating_add(1) >= effective_cols {
            self.blank_wide_wrap_tail(effective_cols, crate::Cell::EMPTY);
            self.advance_autowrap_line();
        }

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);

        let ok = if cursor_col.saturating_add(1) < effective_cols {
            if let Some(row) = self.row_mut(cursor_row) {
                row.write_wide_char(cursor_col, c, fg, bg, flags)
            } else {
                false
            }
        } else {
            false
        };

        if ok {
            self.storage.damage.mark_cell(cursor_row, cursor_col);
            self.storage
                .damage
                .mark_cell(cursor_row, cursor_col.saturating_add(1));

            use std::cmp::Ordering;
            match (self.storage.cursor.col.saturating_add(2)).cmp(&effective_cols) {
                Ordering::Less => {
                    self.storage.cursor.col += 2;
                }
                Ordering::Equal | Ordering::Greater => {
                    self.storage.cursor.col = effective_cols.saturating_sub(1);
                    self.storage.mark_pending_wrap();
                }
            }
        }

        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        ok
    }

    /// Write a wide character without autowrap.
    pub(crate) fn write_wide_char_styled(
        &mut self,
        c: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) -> bool {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);

        let ok = if cursor_col.saturating_add(1) < effective_cols {
            if let Some(row) = self.row_mut(cursor_row) {
                row.write_wide_char(cursor_col, c, fg, bg, flags)
            } else {
                false
            }
        } else {
            false
        };

        if ok {
            self.storage.damage.mark_cell(cursor_row, cursor_col);
            self.storage
                .damage
                .mark_cell(cursor_row, cursor_col.saturating_add(1));

            self.storage.cursor.col = self
                .storage
                .cursor
                .col
                .saturating_add(2)
                .min(effective_cols.saturating_sub(1));
        }

        ok
    }
}
