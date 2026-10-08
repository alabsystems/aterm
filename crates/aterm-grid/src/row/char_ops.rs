// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character insert, delete, and erase operations for Row.
//!
//! These implement the terminal's ICH (Insert Character), DCH (Delete Character),
//! and ECH (Erase Character) operations respectively.

use super::super::CellFlags;
use super::super::cell::Cell;
use super::{Row, RowFlags, u16_from_usize};

impl Row {
    /// Insert `count` blank cells at `col`, shifting existing cells right.
    ///
    /// Cells that would be shifted past the end of the row are discarded.
    /// This implements the ICH (Insert Character) operation.
    #[cfg(any(test, kani))]
    pub fn insert_chars(&mut self, col: u16, count: u16) {
        self.insert_chars_fill(col, count, Cell::EMPTY);
    }

    /// Insert `count` cells filled with `fill` at `col`, shifting existing cells right.
    ///
    /// Like `insert_chars()` but fills the gap with `fill` instead of
    /// `Cell::EMPTY` (BCE).
    pub fn insert_chars_fill(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let old_len = self.len as usize;

        // Wide character fixup: insertion at a boundary that bisects a wide
        // character pair creates orphaned halves that must be cleared.

        // Left boundary: inserting at a wide continuation bisects the pair, so
        // set both halves to `fill`: the head at col-1 and the continuation at
        // col. Clearing only the head would shift the continuation right with a
        // stale WIDE_CONTINUATION bit, and a later edit there would treat the
        // cell to its left as a wide head (e.g. `中\b\x1b[4he#` losing `e`).
        //
        // `is_cell_wide_continuation` requires bit 10 on cells[col] and a WIDE
        // head at col-1: bit 10 aliases PROTECTED, so a raw bit test would fire on
        // a protected cell, and a lone `cells[col-1].WIDE` test on a head whose
        // continuation was overwritten by a narrow cell (`中` then `A`).
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
        }

        // Right boundary: cells at shift_end..cols are discarded. If the
        // last kept cell (shift_end-1) is WIDE, its continuation at
        // shift_end is lost.
        let shift_end = cols.saturating_sub(count);
        if shift_end > 0
            && shift_end < cols
            && self.cells[shift_end - 1].flags().contains(CellFlags::WIDE)
        {
            self.cells[shift_end - 1] = fill;
        }

        // Shift cells right using copy_within (single memmove).
        if shift_end > col {
            self.cells.copy_within(col..shift_end, col + count);
        }

        // Fill the gap with the template cell
        let fill_end = (col + count).min(cols);
        for cell in &mut self.cells[col..fill_end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        if old_len == 0 || col >= old_len {
            // No prior content was shifted. With an EMPTY fill the row stays
            // empty and `len` is unchanged; a BCE (non-empty) fill writes visible
            // background blanks in [col, fill_end), which `len` must count or
            // render_row hides them.
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

    /// Delete `count` cells at `col`, shifting remaining cells left.
    ///
    /// Empty cells are inserted at the end of the row.
    /// This implements the DCH (Delete Character) operation.
    #[cfg(test)]
    pub fn delete_chars(&mut self, col: u16, count: u16) {
        self.delete_chars_fill(col, count, Cell::EMPTY);
    }

    /// Delete `count` cells at `col` with BCE fill, shifting remaining cells left.
    ///
    /// Like `delete_chars()` but fills vacated positions with `fill` instead of
    /// `Cell::EMPTY` (BCE).
    pub fn delete_chars_fill(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let old_len = self.len as usize;

        // Wide character fixup: deletion at a boundary that bisects a wide
        // character pair creates orphaned halves that must be cleared.

        // Left boundary: if col is the continuation of a WIDE head at col-1,
        // deleting it orphans the head, so clear the head too, using the
        // context-aware `is_cell_wide_continuation` (see `insert_chars_fill`).
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

        // Shift cells left using copy_within (single memmove).
        let shift_len = cols - src_start;
        if shift_len > 0 {
            self.cells
                .copy_within(src_start..src_start + shift_len, col);
        }

        // Fill the end with the template cell
        let fill_start = col + shift_len;
        for cell in &mut self.cells[fill_start..] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        if old_len == 0 || col >= old_len {
            // No content beyond the cursor was shifted. With an EMPTY fill the
            // row stays empty; a BCE (non-empty) fill writes visible blanks at the
            // right end ([fill_start, cols)), which `len` must count or render_row
            // hides them. Recalculate over the whole row.
            if !fill.is_empty() {
                self.recalculate_len_up_to(cols);
            }
            return;
        }
        // With a BCE (non-empty) fill the vacated right-margin cells
        // ([cols-count, cols)) are non-empty, so `len` extends to the row edge;
        // the plain `old_len - count` shrink holds only for an EMPTY fill.
        if !fill.is_empty() {
            self.recalculate_len_up_to(cols);
            return;
        }
        let delete_end = col + count;
        if delete_end < old_len {
            // Fast path: a dense tail shrinks by exactly `count`. But the
            // right-boundary wide fixup above clears an extra cell, the deleted
            // head's continuation at src_start == col+count; when that was the
            // row's last content (old_len == col+count+1), the plain shrink leaves
            // len one too high (a phantom trailing space in
            // row_text/search/scrollback). Recalculate only when the new last cell
            // is empty, as erase_chars_with does (never over-shrinks).
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

        // Left-boundary wide fixup via the context-aware
        // `is_cell_wide_continuation` (bit 10 on cells[col] and a WIDE head at
        // col-1). Bit 10 aliases PROTECTED, so a raw WIDE_CONTINUATION test would
        // fire on a protected cell, and a lone `cells[col-1].WIDE` test on a head
        // whose continuation was overwritten by a narrow cell.
        if self.is_cell_wide_continuation(col as u16) {
            // Bisecting a wide pair: set both halves to `fill`. Clearing only the
            // head would let copy_within shift the continuation right with its
            // stale bit 10 (aliasing PROTECTED): a phantom protected spacer that
            // selective erases skip and that keeps the wide char's bg/attrs. As in
            // insert_chars_fill.
            self.cells[col - 1] = fill;
            self.cells[col] = fill;
        }

        // Wide character fixup at right boundary: last kept cell before shift_end
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

        // Shift cells right within [col, right_bound)
        if shift_end > col {
            self.cells.copy_within(col..shift_end, col + count);
        }

        // Fill the gap with the template cell
        let fill_end = (col + count).min(rb);
        for cell in &mut self.cells[col..fill_end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        // Recalculate len since bounded insert may have changed content extent
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

        // Left-boundary wide fixup via the context-aware
        // `is_cell_wide_continuation` (bit 10 on cells[col] and a WIDE head at
        // col-1). Bit 10 aliases PROTECTED, so a raw WIDE_CONTINUATION test would
        // fire on a protected cell, and a lone `cells[col-1].WIDE` test on a head
        // whose continuation was overwritten by a narrow cell.
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
        }

        // Right boundary of deleted region
        let last_deleted = (col + count - 1).min(rb - 1);
        let src_start = (col + count).min(rb);

        // If last deleted cell is WIDE, its continuation becomes orphaned
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

        // Shift cells left within the bounded region
        let shift_len = rb - src_start;
        if shift_len > 0 {
            self.cells
                .copy_within(src_start..src_start + shift_len, col);
        }

        // Fill the end of the bounded region with the template cell
        let fill_start = col + shift_len;
        for cell in &mut self.cells[fill_start..rb] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        // Recalculate len since bounded delete may have changed content extent
        self.recalculate_len_up_to(self.cells.len());
    }

    /// Erase `count` cells starting at `col`, without shifting.
    ///
    /// Cells are replaced with blanks in place. This differs from `delete_chars`
    /// which shifts remaining cells left.
    /// This implements the ECH (Erase Character) operation.
    #[cfg(test)]
    pub fn erase_chars(&mut self, col: u16, count: u16) {
        self.erase_chars_with(col, count, Cell::EMPTY);
    }

    /// Erase `count` cells starting at `col` with a BCE fill cell.
    ///
    /// Like `erase_chars()` but fills with `fill` instead of `Cell::EMPTY`
    /// (BCE per VT420/xterm).
    pub fn erase_chars_with(&mut self, col: u16, count: u16, fill: Cell) {
        if count == 0 || col >= self.cols() {
            return;
        }

        let col = col as usize;
        let count = count as usize;
        let cols = self.cells.len();
        let end = (col + count).min(cols);
        let old_len = self.len as usize;

        // Wide character fixup: erasure at a boundary that bisects a wide
        // character pair creates orphaned halves that must be cleared.

        // Left boundary: if col is the continuation of a WIDE head at col-1,
        // erasing it orphans the head, so clear the head too, using the
        // context-aware `is_cell_wide_continuation` (see `insert_chars_fill`).
        if self.is_cell_wide_continuation(col as u16) {
            self.cells[col - 1] = fill;
        }

        // Right boundary: if the last erased cell is WIDE, its continuation
        // at end is not erased and becomes orphaned.
        if end > col && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
            self.cells[end] = fill;
        }

        for cell in &mut self.cells[col..end] {
            *cell = fill;
        }

        self.flags |= RowFlags::DIRTY;
        if fill.colors() != Cell::EMPTY.colors() {
            // BCE fill: cells have non-default bg, so they are not "empty" for
            // len tracking — renderers need to draw the bg across the range.
            let new_end = end.max(old_len);
            self.len = u16_from_usize(new_end);
        } else if col < old_len && self.cells[old_len - 1].is_empty() {
            // The previous last content cell (old_len - 1) is now empty: it was
            // inside the erase range, or it was the wide continuation cleared at
            // `end` above (which an `end >= old_len` guard would miss).
            // `col < old_len` implies old_len >= 1, so the index is in bounds.
            self.recalculate_len_up_to(old_len);
        }
    }

    /// Selectively erase `count` cells starting at `col`, without shifting.
    ///
    /// Like [`erase_chars`](Self::erase_chars) but skips DECSCA-protected
    /// cells: per VT420/VT510, ECH honours character protection.
    ///
    /// Uses the context-aware [`is_cell_protected`](Self::is_cell_protected)
    /// check to correctly handle wide characters.
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

        // Left boundary: if the first cell is the continuation of an unprotected
        // WIDE cell outside the range, clear that head and the continuation at
        // `col`. Once the head is non-WIDE, is_cell_protected(col) would read
        // the continuation's bit 10 (aliasing PROTECTED) as protected and the
        // loop would skip it, leaving a phantom protected spacer. As in
        // selective_clear_range / selective_wipe_range.
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
                // If this is a WIDE cell, also clear its continuation so the
                // forward iteration doesn't see an orphaned continuation whose
                // WIDE parent was already cleared.
                if self.cells[c].flags().contains(CellFlags::WIDE) && c + 1 < cols {
                    self.cells[c + 1] = fill;
                }
                self.cells[c] = fill;
                any_erased = true;
            }
        }

        if any_erased {
            self.flags |= RowFlags::DIRTY;
            // Not gated on `end >= old_len`: the WIDE-head co-clear above clears
            // cells[c+1], which for c == end-1 is cells[end], one past the range;
            // when that was the tail (old_len == end + 1) such a gate would leave
            // len stale-high. recalculate_len_up_to(old_len) never over-shrinks.
            if col < old_len && self.cells[old_len - 1].is_empty() {
                self.recalculate_len_up_to(old_len);
            }
        }
    }
}
