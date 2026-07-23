// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Test-only Grid write convenience methods.
//!
//! These combined write+advance methods are superseded by the split
//! write/advance primitives in `write_split.rs` for production use.
//! Retained here for test readability.

use super::{CellFlags, Grid, PackedColor, StyleId};

impl Grid {
    /// Write a styled character at cursor position and advance cursor.
    ///
    /// Production code uses `write_split::write_char_at_cursor` + `advance_cursor_*` instead.
    /// Retained for test convenience.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
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

        // Advance cursor
        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        }
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Write a character using a style ID. Delegates to `write_char_styled`.
    pub(crate) fn write_char_with_style_id(
        &mut self,
        c: char,
        style_id: StyleId,
        extra_flags: CellFlags,
    ) {
        let (fg, bg, flags) = self.resolve_style_to_colors(style_id, extra_flags);
        self.write_char_styled(c, fg, bg, flags);
    }

    /// Write a styled character with autowrap.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.row < self.storage.visible_rows
    pub(crate) fn write_char_wrap_styled(
        &mut self,
        c: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) {
        // Resolve any deferred wrap before writing
        self.resolve_pending_wrap();

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char_styled(cursor_col, c, fg, bg, flags);
        }
        self.storage.damage.mark_cell(cursor_row, cursor_col);

        // Advance cursor with deferred wrap
        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        } else {
            self.storage.mark_pending_wrap();
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Write a character with autowrap using a style ID.
    pub(crate) fn write_char_wrap_with_style_id(
        &mut self,
        c: char,
        style_id: StyleId,
        extra_flags: CellFlags,
    ) {
        let (fg, bg, flags) = self.resolve_style_to_colors(style_id, extra_flags);
        self.write_char_wrap_styled(c, fg, bg, flags);
    }

    /// Write a wide (2-cell) character with autowrap and deferred wrapping.
    /// Returns `true` if written, `false` if cannot fit.
    pub(crate) fn write_wide_char_wrap_styled(
        &mut self,
        c: char,
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
    ) -> bool {
        // Resolve any deferred wrap before writing
        self.resolve_pending_wrap();

        // If we're at the last column, we need to wrap first
        // (wide char can't start at last column)
        let effective_cols = self.storage.effective_cols_for_row(self.storage.cursor.row);
        if self.storage.cursor.col.saturating_add(1) >= effective_cols {
            self.advance_autowrap_line();
        }

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);

        // Write the wide character (main cell + continuation)
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

            // Advance cursor by 2 with deferred wrap
            use std::cmp::Ordering;
            match (self.storage.cursor.col.saturating_add(2)).cmp(&effective_cols) {
                Ordering::Less => {
                    self.storage.cursor.col += 2;
                }
                Ordering::Equal | Ordering::Greater => {
                    // At or past end of line — defer wrap
                    self.storage.cursor.col = effective_cols.saturating_sub(1);
                    self.storage.mark_pending_wrap();
                }
            }
        }

        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        ok
    }

    /// Write a wide (2-cell) character without autowrap. Returns `true` or `false`.
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

        // Write the wide character
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

            // Advance cursor by 2, but don't exceed bounds
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
