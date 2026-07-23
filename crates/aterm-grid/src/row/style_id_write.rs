// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! StyleId-based write methods for Row.
//!
//! These methods are only available in test and Kani contexts where StyleId
//! is the preferred way to style cells (vs inline colors in production).

use super::super::CellFlags;
use super::super::cell::Cell;
use super::super::style::StyleId;
use super::{Row, RowFlags};

impl Row {
    /// Write a styled character at the given column using StyleId.
    ///
    /// This is the StyleId variant of `write_char_styled`. Instead of storing
    /// inline colors, the cell stores a StyleId that references the StyleTable.
    /// If overwriting part of a wide character, the orphaned half is cleared to space.
    #[inline]
    pub fn write_char_with_style_id(
        &mut self,
        col: u16,
        c: char,
        style_id: StyleId,
        cell_flags: CellFlags,
    ) -> bool {
        let col_usize = col as usize;
        let cells_len = self.cells.len();
        let old_len = self.len as usize;

        // Single bounds check upfront
        if col_usize >= cells_len {
            return false;
        }

        // SAFETY: col_usize < cells_len verified above
        let current_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();

        // Wide character fixup (rare path) - check if either WIDE or WIDE_CONTINUATION is set
        let wide_mask = CellFlags::WIDE.union(CellFlags::WIDE_CONTINUATION);
        if (current_flags.bits() & wide_mask.bits()) != 0 {
            self.fixup_wide_char_overwrite(col_usize, current_flags, cells_len);
        }

        // SAFETY: col_usize < cells_len verified above
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) = Cell::with_style_id(c, style_id, cell_flags);
        }
        if col >= self.len {
            self.len = col.saturating_add(1);
        } else if old_len > 0 && self.cells[old_len - 1].is_empty() {
            // Interior single-width write over a WIDE head at the tail cleared its
            // continuation via fixup_wide_char_overwrite; if that was the last
            // content (old_len == col+2), len would be stale-high. #7522 len class,
            // mirrored from write_char_packed (recalculate never over-shrinks).
            // Ungated (unlike write_char_packed): this variant's fixup is itself
            // ungated, and write_wide_char_with_style_id doesn't set HAS_WIDE_CHARS,
            // so the recalc must match the fixup's condition, not the fast-path flag.
            self.recalculate_len_up_to(old_len);
        }
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_STYLE_ID;
        true
    }

    /// Write a wide (double-width) character at the given column using StyleId.
    ///
    /// This is the StyleId variant of `write_wide_char`. Instead of storing
    /// inline colors, the cells store a StyleId that references the StyleTable.
    /// Wide characters occupy two cells. The first cell contains the character
    /// with the WIDE flag set, and the second cell is a continuation cell.
    /// If overwriting parts of other wide characters, the orphaned halves are cleared.
    ///
    /// Returns `true` if the write succeeded, `false` if out of bounds.
    #[inline]
    pub fn write_wide_char_with_style_id(
        &mut self,
        col: u16,
        c: char,
        style_id: StyleId,
        cell_flags: CellFlags,
    ) -> bool {
        let cells_len = self.cells.len();
        let col_usize = col as usize;
        let old_len = self.len as usize;

        // Need at least 2 cells available - single bounds check
        if col_usize + 1 >= cells_len {
            // Not enough room - write to last column as single-width
            // (this matches terminal behavior when wide char is at edge)
            return false;
        }

        // SAFETY: col_usize < cells_len and col_usize + 1 < cells_len verified above
        let first_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
        let second_flags = unsafe { self.cells.get_unchecked(col_usize + 1) }.flags();

        // Wide character fixup (rare path)
        if first_flags.contains(CellFlags::WIDE_CONTINUATION)
            || second_flags.contains(CellFlags::WIDE)
        {
            self.fixup_wide_char_write(col_usize, second_flags, cells_len);
        }

        // SAFETY: bounds already verified
        unsafe {
            // Write main cell with WIDE flag
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::with_style_id(c, style_id, cell_flags.union(CellFlags::WIDE));
            // Write continuation cell
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::with_style_id(' ', style_id, CellFlags::WIDE_CONTINUATION);
        }

        if col + 1 >= self.len {
            self.len = col.saturating_add(2);
        } else if old_len > 0 && self.cells[old_len - 1].is_empty() {
            // Interior wide write orphaned a pre-existing wide char's continuation
            // at col+2 (fixup_wide_char_write); if that was the last content
            // (old_len == col+3), len would be stale-high. #7522 len class,
            // mirrored from write_wide_char_packed (recalculate never over-shrinks).
            self.recalculate_len_up_to(old_len);
        }
        // Set HAS_WIDE_CHARS (matching the production write_wide_char_packed):
        // without it a later HAS_WIDE_CHARS-gated fixup (e.g. write_char /
        // write_char_packed) would skip its wide cleanup on this row, leaving a
        // live orphaned continuation AND a stale-high len when the head is
        // overwritten. The flag is the row-level fast-path predicate for wide
        // fixup, so every wide write must set it.
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_STYLE_ID | RowFlags::HAS_WIDE_CHARS;
        true
    }
}
