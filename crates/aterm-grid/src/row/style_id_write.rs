// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! StyleId-based write methods for Row (test only; production stores inline
//! colours).

use super::super::CellFlags;
use super::super::cell::Cell;
use super::super::style::StyleId;
use super::{Row, RowFlags};

impl Row {
    /// Write a character at `col` referencing a StyleTable entry. An overwritten
    /// wide character's orphaned half is cleared to space.
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

        if col_usize >= cells_len {
            return false;
        }

        // SAFETY: col_usize < cells_len verified above
        let current_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();

        // Wide fixup only when the cell is either half of a wide pair (rare).
        let wide_mask = CellFlags::WIDE.union(CellFlags::WIDE_CONTINUATION);
        let end_half_blanked = (current_flags.bits() & wide_mask.bits()) != 0
            && self.fixup_wide_char_overwrite(col_usize, current_flags, cells_len);

        // SAFETY: col_usize < cells_len verified above
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) = Cell::with_style_id(c, style_id, cell_flags);
        }
        self.settle_len_after_write(old_len, col_usize + 1, end_half_blanked);
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_STYLE_ID;
        true
    }

    /// Write a wide character at `col` referencing a StyleTable entry: the
    /// first cell carries WIDE, the second is its continuation. Orphaned halves
    /// of overwritten wide chars are cleared.
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

        // A wide glyph needs two cells; at the edge the write is refused.
        if col_usize + 1 >= cells_len {
            return false;
        }

        // SAFETY: col_usize < cells_len and col_usize + 1 < cells_len verified above
        let first_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
        let second_flags = unsafe { self.cells.get_unchecked(col_usize + 1) }.flags();

        let end_half_blanked = (first_flags.contains(CellFlags::WIDE_CONTINUATION)
            || second_flags.contains(CellFlags::WIDE))
            && self.fixup_wide_char_write(col_usize, second_flags, cells_len);

        // SAFETY: col_usize + 1 < cells_len, verified above.
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::with_style_id(c, style_id, cell_flags.union(CellFlags::WIDE));
            // The continuation carries the lead's rendition; `with_style_id` adds
            // USES_STYLE_ID itself, so `wide_continuation_of` masks bits 14-15.
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::with_style_id(' ', style_id, cell_flags.wide_continuation_of());
        }

        self.settle_len_after_write(old_len, col_usize + 2, end_half_blanked);
        // HAS_WIDE_CHARS gates the row's wide fixups, so every wide write sets it.
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_STYLE_ID | RowFlags::HAS_WIDE_CHARS;
        true
    }
}
