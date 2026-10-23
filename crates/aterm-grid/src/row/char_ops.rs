// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row character insert, delete and erase: ICH, DCH, ECH.

use super::super::CellFlags;
use super::super::cell::Cell;
use super::{BlankedHalves, Row, RowFlags, u16_from_usize};

impl Row {
    /// Insert `count` `Cell::EMPTY` cells at `col`, shifting existing cells right.
    #[cfg(test)]
    pub fn insert_chars(&mut self, col: u16, count: u16) {
        let _ = self.insert_chars_fill(col, count, Cell::EMPTY);
    }

    /// Insert `count` cells filled with `fill` at `col`, shifting existing cells right (ICH).
    pub fn insert_chars_fill(&mut self, col: u16, count: u16, fill: Cell) -> BlankedHalves {
        let mut blanked = BlankedHalves::default();
        if count == 0 || col >= self.cols() {
            return blanked;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let old_len = self.len as usize;
        let shift_end = cols.saturating_sub(count);

        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
            blanked[0] = Some(u16_from_usize(col - 1));
            if shift_end > col {
                blanked[1] = Some(u16_from_usize(col + count));
            }
        }

        if shift_end > 0
            && shift_end < cols
            && self.cells[shift_end - 1].flags().contains(CellFlags::WIDE)
        {
            self.cells[shift_end - 1] = fill;
            let head = shift_end - 1;
            blanked[2] = Some(u16_from_usize(if head >= col {
                head + count
            } else {
                head
            }));
        }

        if shift_end > col {
            self.cells.copy_within(col..shift_end, col + count);
        }

        let fill_end = (col + count).min(cols);
        for cell in &mut self.cells[col..fill_end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        self.note_write_through(cols);
        if old_len == 0 || col >= old_len {
            if !fill.is_empty() {
                self.recalculate_len_up_to(fill_end);
            }
            return blanked;
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
        blanked
    }

    /// Delete `count` cells at `col`, filling the vacated end with `Cell::EMPTY`.
    #[cfg(test)]
    pub fn delete_chars(&mut self, col: u16, count: u16) {
        let _ = self.delete_chars_fill(col, count, Cell::EMPTY);
    }

    /// Delete `count` cells at `col`, shifting left and filling vacated cells with `fill` (DCH).
    pub fn delete_chars_fill(&mut self, col: u16, count: u16, fill: Cell) -> BlankedHalves {
        let mut blanked = BlankedHalves::default();
        if count == 0 || col >= self.cols() {
            return blanked;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let old_len = self.len as usize;

        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            blanked[0] = Some(u16_from_usize(col - 1));
        }

        let last_deleted = (col + count - 1).min(cols - 1);
        let src_start = (col + count).min(cols);
        if self.cells[last_deleted].flags().contains(CellFlags::WIDE) && src_start < cols {
            self.cells[src_start] = fill;
            blanked[1] = Some(u16_from_usize(col));
        }

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
        self.note_write_through(cols);
        if old_len == 0 || col >= old_len {
            if !fill.is_empty() {
                self.recalculate_len_up_to(cols);
            }
            return blanked;
        }
        if !fill.is_empty() {
            self.recalculate_len_up_to(cols);
            return blanked;
        }
        let delete_end = col + count;
        if delete_end < old_len {
            let cand = old_len - count;
            self.len = u16_from_usize(cand);
            if cand > 0 && self.cells[cand - 1].is_empty() {
                self.recalculate_len_up_to(cand);
            }
        } else {
            self.recalculate_len_up_to(col);
        }
        blanked
    }

    /// Insert `count` cells at `col`, shifting cells right up to `right_bound`, with BCE fill.
    pub fn insert_chars_bounded_fill(
        &mut self,
        col: u16,
        count: u16,
        right_bound: u16,
        fill: Cell,
    ) -> BlankedHalves {
        let mut blanked = BlankedHalves::default();
        let right_bound = right_bound.min(self.cols());
        if count == 0 || col >= right_bound {
            return blanked;
        }

        let col = col as usize;
        let count = count as usize;
        let rb = right_bound as usize;
        let shift_end = rb.saturating_sub(count);

        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
            blanked[0] = Some(u16_from_usize(col - 1));
            if shift_end > col {
                blanked[1] = Some(u16_from_usize(col + count));
            }
        }

        if shift_end > col
            && shift_end < rb
            && self.cells[shift_end - 1].flags().contains(CellFlags::WIDE)
        {
            self.cells[shift_end - 1] = fill;
            blanked[2] = Some(u16_from_usize(rb - 1));
        }

        if rb < self.cells.len() && self.cells[rb - 1].flags().contains(CellFlags::WIDE) {
            self.cells[rb] = fill;
            blanked[3] = Some(u16_from_usize(rb));
        }

        if shift_end > col {
            self.cells.copy_within(col..shift_end, col + count);
        }

        let fill_end = (col + count).min(rb);
        for cell in &mut self.cells[col..fill_end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        self.note_write_through(rb);
        self.recalculate_len_up_to(self.cells.len());
        blanked
    }

    /// Delete `count` cells at `col`, shifting cells left within `right_bound`, with BCE fill.
    pub fn delete_chars_bounded_fill(
        &mut self,
        col: u16,
        count: u16,
        right_bound: u16,
        fill: Cell,
    ) -> BlankedHalves {
        let mut blanked = BlankedHalves::default();
        let right_bound = right_bound.min(self.cols());
        if count == 0 || col >= right_bound {
            return blanked;
        }

        let col = col as usize;
        let count = count as usize;
        let rb = right_bound as usize;

        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            blanked[0] = Some(u16_from_usize(col - 1));
        }

        let last_deleted = (col + count - 1).min(rb - 1);
        let src_start = (col + count).min(rb);

        if self.cells[last_deleted].flags().contains(CellFlags::WIDE) && src_start < rb {
            self.cells[src_start] = fill;
            blanked[1] = Some(u16_from_usize(col));
        }

        if rb < self.cells.len() && self.cells[rb - 1].flags().contains(CellFlags::WIDE) {
            self.cells[rb] = fill;
            blanked[2] = Some(u16_from_usize(rb));
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
        self.note_write_through(rb);
        self.recalculate_len_up_to(self.cells.len());
        blanked
    }

    /// Erase `count` cells from `col` in place with `Cell::EMPTY`.
    #[cfg(test)]
    pub fn erase_chars(&mut self, col: u16, count: u16) {
        let _ = self.erase_chars_with(col, count, Cell::EMPTY);
    }

    /// Erase `count` cells from `col` in place (ECH) with the BCE `fill` (VT420/xterm).
    pub fn erase_chars_with(&mut self, col: u16, count: u16, fill: Cell) -> BlankedHalves {
        let mut blanked = BlankedHalves::default();
        if count == 0 || col >= self.cols() {
            return blanked;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let end = (col + count).min(cols);
        let old_len = self.len as usize;

        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            blanked[0] = Some(u16_from_usize(col - 1));
        }

        if end > col && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
            self.cells[end] = fill;
            blanked[1] = Some(u16_from_usize(end));
        }

        for cell in &mut self.cells[col..end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        if fill.colors() != Cell::EMPTY.colors() {
            let new_end = end.max(old_len);
            self.len = u16_from_usize(new_end);
        } else if col < old_len && self.cells[old_len - 1].is_empty() {
            self.recalculate_len_up_to(old_len);
        }
        blanked
    }

    /// [`erase_chars`](Self::erase_chars), skipping DECSCA-protected cells.
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
                if self.cells[c].flags().contains(CellFlags::WIDE) && c + 1 < cols {
                    self.cells[c + 1] = fill;
                }
                self.cells[c] = fill;
                any_erased = true;
            }
        }

        if any_erased {
            self.flags |= RowFlags::DIRTY;
            if col < old_len && self.cells[old_len - 1].is_empty() {
                self.recalculate_len_up_to(old_len);
            }
        }
    }
}
