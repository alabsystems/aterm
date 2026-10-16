// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row character insert, delete and erase: ICH, DCH, ECH.

use super::super::CellFlags;
use super::super::cell::Cell;
use super::{Row, RowFlags, u16_from_usize};

impl Row {
    /// Insert `count` `Cell::EMPTY` cells at `col`, shifting existing cells right.
    #[cfg(test)]
    pub fn insert_chars(&mut self, col: u16, count: u16) {
        self.insert_chars_fill(col, count, Cell::EMPTY);
    }

    /// Insert `count` cells filled with the BCE `fill` at `col`, shifting
    /// existing cells right (ICH); cells pushed past the row end are dropped.
    pub fn insert_chars_fill(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let old_len = self.len as usize;

        // Left boundary: inserting at a wide continuation bisects the pair, so
        // both halves get `fill`. Clearing only the head would shift the
        // continuation right with a stale WIDE_CONTINUATION bit, and a later
        // edit there would treat its left neighbour as a wide head.
        //
        // `is_cell_wide_continuation` needs bit 10 on cells[col] and a WIDE head
        // at col-1: bit 10 aliases PROTECTED, so a raw bit test fires on a
        // protected cell, and a lone `cells[col-1].WIDE` test on a head whose
        // continuation a narrow cell overwrote.
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
        }

        // Right boundary: cells from shift_end are discarded, so a WIDE head at
        // shift_end-1 loses its continuation.
        let shift_end = cols.saturating_sub(count);
        if shift_end > 0
            && shift_end < cols
            && self.cells[shift_end - 1].flags().contains(CellFlags::WIDE)
        {
            self.cells[shift_end - 1] = fill;
        }

        if shift_end > col {
            self.cells.copy_within(col..shift_end, col + count);
        }

        let fill_end = (col + count).min(cols);
        for cell in &mut self.cells[col..fill_end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        // The shift rewrote the last cell: an early-wrap hole there is gone.
        self.note_write_through(cols);
        if old_len == 0 || col >= old_len {
            // Nothing was shifted. A BCE fill's blanks at [col, fill_end) are
            // content `len` must count, or render_row hides them.
            if !fill.is_empty() {
                self.recalculate_len_up_to(fill_end);
            }
            return;
        }
        let new_len = old_len + count;
        if new_len <= cols {
            self.len = u16_from_usize(new_len);
        } else if cols > 0 {
            if self.cells[cols - 1].is_empty() {
                self.recalculate_len_up_to(cols);
            } else {
                self.len = u16_from_usize(cols);
            }
        } else {
            self.len = 0;
        }
    }

    /// Delete `count` cells at `col`, filling the vacated end with `Cell::EMPTY`.
    #[cfg(test)]
    pub fn delete_chars(&mut self, col: u16, count: u16) {
        self.delete_chars_fill(col, count, Cell::EMPTY);
    }

    /// Delete `count` cells at `col`, shifting the rest left and filling the
    /// vacated end with the BCE `fill` (DCH).
    pub fn delete_chars_fill(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let old_len = self.len as usize;

        // Left boundary: deleting a continuation orphans its head at col-1
        // (context-aware check, see `insert_chars_fill`).
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
        }

        // Right boundary: if the last deleted cell is WIDE, its continuation
        // at col+count shifts left and becomes orphaned.
        let last_deleted = (col + count - 1).min(cols - 1);
        let src_start = (col + count).min(cols);
        if self.cells[last_deleted].flags().contains(CellFlags::WIDE) && src_start < cols {
            self.cells[src_start] = fill;
        }
        // No separate WIDE_CONTINUATION check on cells[src_start]: a genuine
        // orphan there has its head at src_start-1 == last_deleted, handled above,
        // and a raw bit test would fire on a protected cell shifting into col.

        let shift_len = cols - src_start;
        if shift_len > 0 {
            self.cells
                .copy_within(src_start..src_start + shift_len, col);
        }

        let fill_start = col + shift_len;
        for cell in &mut self.cells[fill_start..] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        // The shift rewrote the last cell: an early-wrap hole there is gone.
        self.note_write_through(cols);
        if old_len == 0 || col >= old_len {
            // Nothing was shifted. A BCE fill's blanks at [fill_start, cols)
            // are content `len` must count, or render_row hides them.
            if !fill.is_empty() {
                self.recalculate_len_up_to(cols);
            }
            return;
        }
        // A BCE fill makes the vacated right-margin cells content, so `len`
        // reaches the row edge; `old_len - count` holds only for EMPTY.
        if !fill.is_empty() {
            self.recalculate_len_up_to(cols);
            return;
        }
        let delete_end = col + count;
        if delete_end < old_len {
            // A dense tail shrinks by exactly `count`, unless the right
            // boundary fixup also cleared the deleted head's continuation and
            // that was the last content (old_len == col+count+1): recalculate
            // when the new last cell is empty (never over-shrinks).
            let cand = old_len - count;
            self.len = u16_from_usize(cand);
            if cand > 0 && self.cells[cand - 1].is_empty() {
                self.recalculate_len_up_to(cand);
            }
        } else {
            self.recalculate_len_up_to(col);
        }
    }

    /// Insert `count` cells at `col`, shifting cells right up to `right_bound`,
    /// with BCE fill. Cells shifted past `right_bound` are discarded; used when
    /// DECLRMM horizontal margins restrict the shift region.
    pub fn insert_chars_bounded_fill(
        &mut self,
        col: u16,
        count: u16,
        right_bound: u16,
        fill: Cell,
    ) {
        let right_bound = right_bound.min(self.cols());
        if count == 0 || col >= right_bound {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let rb = right_bound as usize;

        // Context-aware left-boundary fixup; both halves get `fill` (see
        // `insert_chars_fill`).
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
        }

        // Right boundary: a WIDE head at shift_end-1 loses its continuation,
        // which the shift discards.
        let shift_end = rb.saturating_sub(count);
        if shift_end > col
            && shift_end < rb
            && self.cells[shift_end - 1].flags().contains(CellFlags::WIDE)
        {
            self.cells[shift_end - 1] = fill;
        }

        // Wide fixup at the outer right boundary: if cells[rb-1] is WIDE, its
        // continuation at cells[rb] is outside the region and is orphaned when
        // the shift overwrites cells[rb-1].
        if rb < self.cells.len() && self.cells[rb - 1].flags().contains(CellFlags::WIDE) {
            self.cells[rb] = fill;
        }

        if shift_end > col {
            self.cells.copy_within(col..shift_end, col + count);
        }

        let fill_end = (col + count).min(rb);
        for cell in &mut self.cells[col..fill_end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        // A shift through the row end rewrote the last cell.
        self.note_write_through(rb);
        self.recalculate_len_up_to(self.cells.len());
    }

    /// Delete `count` cells at `col`, shifting cells left within `right_bound`,
    /// with BCE fill.
    pub fn delete_chars_bounded_fill(
        &mut self,
        col: u16,
        count: u16,
        right_bound: u16,
        fill: Cell,
    ) {
        let right_bound = right_bound.min(self.cols());
        if count == 0 || col >= right_bound {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let rb = right_bound as usize;

        // Context-aware left-boundary fixup (see `insert_chars_fill`).
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
        }

        let last_deleted = (col + count - 1).min(rb - 1);
        let src_start = (col + count).min(rb);

        // The last deleted cell's continuation is orphaned.
        if self.cells[last_deleted].flags().contains(CellFlags::WIDE) && src_start < rb {
            self.cells[src_start] = fill;
        }
        // No separate WIDE_CONTINUATION check on cells[src_start]: a genuine
        // orphan there has its head at src_start-1 == last_deleted, handled
        // above, and a raw bit test would fire on a protected cell.

        // Wide fixup at the outer right boundary: if cells[rb-1] is WIDE, its
        // continuation at cells[rb] is outside the region and is orphaned when
        // the fill overwrites cells[rb-1].
        if rb < self.cells.len() && self.cells[rb - 1].flags().contains(CellFlags::WIDE) {
            self.cells[rb] = fill;
        }

        let shift_len = rb - src_start;
        if shift_len > 0 {
            self.cells
                .copy_within(src_start..src_start + shift_len, col);
        }

        let fill_start = col + shift_len;
        for cell in &mut self.cells[fill_start..rb] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        // A shift through the row end rewrote the last cell.
        self.note_write_through(rb);
        self.recalculate_len_up_to(self.cells.len());
    }

    /// Erase `count` cells from `col` in place with `Cell::EMPTY`.
    #[cfg(test)]
    pub fn erase_chars(&mut self, col: u16, count: u16) {
        self.erase_chars_with(col, count, Cell::EMPTY);
    }

    /// Erase `count` cells from `col` in place (ECH) with the BCE `fill`
    /// (VT420/xterm).
    pub fn erase_chars_with(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let end = (col + count).min(cols);
        let old_len = self.len as usize;

        // Left boundary: erasing a continuation orphans its head at col-1
        // (context-aware check, see `insert_chars_fill`).
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
        }

        // Right boundary: the last erased cell's continuation at `end` is
        // orphaned.
        if end > col && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
            self.cells[end] = fill;
        }

        for cell in &mut self.cells[col..end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        if fill.colors() != Cell::EMPTY.colors() {
            // A BCE background is content: renderers draw it across the range.
            let new_end = end.max(old_len);
            self.len = u16_from_usize(new_end);
        } else if col < old_len && self.cells[old_len - 1].is_empty() {
            // The old last content cell is now empty: it was in the range, or
            // it was the wide continuation cleared at `end` (which an
            // `end >= old_len` guard would miss).
            self.recalculate_len_up_to(old_len);
        }
    }

    /// [`erase_chars`](Self::erase_chars), skipping DECSCA-protected cells:
    /// per VT420/VT510, ECH honours character protection.
    #[cfg(test)]
    pub fn selective_erase_chars(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let end = (col + count).min(cols);
        let old_len = self.len as usize;
        let mut any_erased = false;

        // Left boundary: a continuation of an unprotected WIDE head outside the
        // range is cleared with its head here; once the head is non-WIDE, the
        // loop would read the continuation's bit 10 as PROTECTED and skip it.
        if col > 0
            && self.cells[col]
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION)
            && self.cells[col - 1].flags().contains(CellFlags::WIDE)
            && !self.is_cell_protected((col - 1) as u16)
        {
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
            any_erased = true;
        }

        for c in col..end {
            if !self.is_cell_protected(c as u16) {
                // A cleared WIDE head takes its continuation with it.
                if self.cells[c].flags().contains(CellFlags::WIDE) && c + 1 < cols {
                    self.cells[c + 1] = fill;
                }
                self.cells[c] = fill;
                any_erased = true;
            }
        }

        if any_erased {
            self.flags |= RowFlags::DIRTY;
            // Not gated on `end >= old_len` (see `selective_clear_range`).
            if col < old_len && self.cells[old_len - 1].is_empty() {
                self.recalculate_len_up_to(old_len);
            }
        }
    }
}
