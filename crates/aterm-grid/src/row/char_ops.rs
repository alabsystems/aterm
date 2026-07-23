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
    pub fn insert_chars(&mut self, col: u16, count: u16) {
        self.insert_chars_fill(col, count, Cell::EMPTY);
    }

    /// Insert `count` cells filled with `fill` at `col`, shifting existing cells right.
    ///
    /// Like `insert_chars()` but fills the gap with `fill` instead of `Cell::EMPTY`,
    /// supporting BCE (Background Color Erase) (#7522).
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

        // Left boundary: if inserting at a WIDE_CONTINUATION, the insertion
        // bisects the pair. Orphan BOTH halves to `fill`: the WIDE head at
        // col-1, AND the continuation at col itself. Clearing only the head
        // would shift the continuation right (below) still carrying its stale
        // WIDE_CONTINUATION flag — a later insert/erase at that cell would then
        // treat the unrelated cell to its left as a wide head and wrongly clear
        // it (e.g. `中\b\x1b[4he#` lost the inserted `e`).
        //
        // Use the context-aware `is_cell_wide_continuation`: bit 10 (WIDE_CONTINUATION)
        // aliases PROTECTED, so neither a raw `cells[col].WIDE_CONTINUATION` test (fires
        // on a DECSCA-protected cell at col) nor a lone `cells[col-1].WIDE` test (fires on
        // a dangling WIDE head whose continuation was overwritten by a narrow cell, e.g.
        // `中` then `A` at col, losing the `A`) is correct alone. The helper requires BOTH
        // bit 10 on cells[col] AND a WIDE head at col-1 (round-5 DECCRA / round-7/8
        // aliasing-class fix).
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
            // empty, so `len` is unchanged. But a BCE (non-empty) fill writes
            // visible background-colored blanks in [col, fill_end); those must
            // be counted in `len` or render_row hides them (xterm/alacritty
            // show them). Recalculate over the filled region (#7522 gap).
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
    pub fn delete_chars(&mut self, col: u16, count: u16) {
        self.delete_chars_fill(col, count, Cell::EMPTY);
    }

    /// Delete `count` cells at `col` with BCE fill, shifting remaining cells left.
    ///
    /// Like `delete_chars()` but fills vacated positions with `fill` instead of
    /// `Cell::EMPTY`, supporting BCE (Background Color Erase) (#7522).
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

        // Left boundary: if col is the continuation of a WIDE head at col-1, deleting
        // it orphans the head — clear the head too. Use the context-aware
        // `is_cell_wide_continuation` (requires bit 10 on cells[col] AND a WIDE head at
        // col-1): a lone `cells[col-1].WIDE` test would blank a live head whose
        // continuation was overwritten by a narrow cell at col, and a raw
        // `cells[col].WIDE_CONTINUATION` test would fire on a DECSCA-protected cell at col
        // (bit-10 aliasing; round-7/8 fix).
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
        // NOTE: no separate WIDE_CONTINUATION check on cells[src_start] — a genuine
        // orphaned continuation there always has its WIDE head at src_start-1 ==
        // last_deleted, already handled by the WIDE check above. A raw
        // WIDE_CONTINUATION test here would only ever fire (falsely) on a
        // DECSCA-protected cell shifting into col and silently destroy it.

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
            // No prior content beyond the cursor was shifted. With an EMPTY
            // fill the row stays empty. A BCE (non-empty) fill writes visible
            // background-colored blanks at the right end ([fill_start, cols));
            // those must be counted in `len` or render_row hides them
            // (xterm/alacritty show them). Recalculate over the whole row
            // (#7522 gap).
            if !fill.is_empty() {
                self.recalculate_len_up_to(cols);
            }
            return;
        }
        // With a BCE (non-empty) fill the vacated right-margin cells
        // ([cols-count, cols)) are background-colored and therefore non-empty,
        // so `len` must extend to the row edge instead of shrinking by `count`.
        // The plain `old_len - count` shrink is only valid for an EMPTY fill
        // (#7522 gap).
        if !fill.is_empty() {
            self.recalculate_len_up_to(cols);
            return;
        }
        let delete_end = col + count;
        if delete_end < old_len {
            // Fast path: a dense tail shrinks by exactly `count`. But the
            // right-boundary wide fixup above (lines 146-149) clears an EXTRA cell
            // — the deleted WIDE head's orphaned continuation at src_start ==
            // col+count. When that continuation was the row's last content
            // (old_len == col+count+1), the shift moves the now-empty cell into the
            // tail, so the true extent is one less than old_len - count and the
            // plain shrink leaves len stale-high (phantom trailing space through
            // row_text/search/scrollback). Guard exactly like erase_chars_with:
            // recalc only when the new last cell turns out empty (never
            // over-shrinks, preserves the dense-tail fast path). #7522 write/erase
            // len class — delete_chars_fill was the last DCH primitive missing it.
            let cand = old_len - count;
            self.len = u16_from_usize(cand);
            if cand > 0 && self.cells[cand - 1].is_empty() {
                self.recalculate_len_up_to(cand);
            }
        } else {
            self.recalculate_len_up_to(col);
        }
    }

    /// Insert `count` blank cells at `col`, shifting cells right up to `right_bound`.
    ///
    /// Only cells in [col, right_bound) are affected. Cells shifted past
    /// `right_bound` are discarded. Used when DECLRMM horizontal margins
    /// restrict the shift region (#7320).
    pub fn insert_chars_bounded(&mut self, col: u16, count: u16, right_bound: u16) {
        self.insert_chars_bounded_fill(col, count, right_bound, Cell::EMPTY);
    }

    /// Insert with BCE fill. See `insert_chars_bounded` (#7522).
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

        // Wide character fixup at left boundary. Use the context-aware
        // `is_cell_wide_continuation` (bit 10 on cells[col] AND a WIDE head at col-1):
        // neither a lone `cells[col-1].WIDE` test (blanks a live head whose continuation
        // was overwritten) nor a raw `cells[col].WIDE_CONTINUATION` test (fires on a
        // DECSCA-protected cell at col, whose col-1 is OUTSIDE the region) is correct
        // alone (bit-10 aliasing; round-7/8 fix).
        if self.is_cell_wide_continuation(col as u16) {
            // Bisecting a wide pair: orphan BOTH halves to `fill`. Clearing only
            // the head at col-1 would leave the continuation at col carrying its
            // stale WIDE_CONTINUATION bit (bit 10, aliases PROTECTED) to be shifted
            // right by copy_within below — a phantom protected spacer that a later
            // selective erase (DECSED/DECSEL) then skips, and which keeps the wide
            // char's bg/attrs. Mirror insert_chars_fill, which clears both halves.
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

        // Wide character fixup at outer right boundary: if cells[rb-1] is WIDE,
        // its continuation at cells[rb] is outside the bounded region and will
        // be orphaned when cells[rb-1] is overwritten by the shift (#7492).
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

    /// Delete `count` cells at `col`, shifting cells left up to `right_bound`.
    ///
    /// Only cells in [col, right_bound) are affected. Blank cells are
    /// inserted at the right end of the bounded region. Used when DECLRMM
    /// horizontal margins restrict the shift region (#7320).
    pub fn delete_chars_bounded(&mut self, col: u16, count: u16, right_bound: u16) {
        self.delete_chars_bounded_fill(col, count, right_bound, Cell::EMPTY);
    }

    /// Delete with BCE fill. See `delete_chars_bounded` (#7522).
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

        // Wide character fixup at left boundary. Use the context-aware
        // `is_cell_wide_continuation` (bit 10 on cells[col] AND a WIDE head at col-1):
        // neither a lone `cells[col-1].WIDE` test (blanks a live head whose continuation
        // was overwritten) nor a raw `cells[col].WIDE_CONTINUATION` test (fires on a
        // DECSCA-protected cell at col, whose col-1 is OUTSIDE the region) is correct
        // alone (bit-10 aliasing; round-7/8 fix).
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
        // NOTE: no separate WIDE_CONTINUATION check on cells[src_start] — a genuine
        // orphan there has its WIDE head at src_start-1 == last_deleted, already
        // handled above. A raw WIDE_CONTINUATION test would only misfire on a
        // DECSCA-protected cell shifting into col and silently destroy it.

        // Wide character fixup at outer right boundary: if cells[rb-1] is WIDE,
        // its continuation at cells[rb] is outside the bounded region and will
        // be orphaned when cells[rb-1] is overwritten by the fill (#7492).
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
    pub fn erase_chars(&mut self, col: u16, count: u16) {
        self.erase_chars_with(col, count, Cell::EMPTY);
    }

    /// Erase `count` cells starting at `col` with a BCE fill cell (#7522).
    ///
    /// Like `erase_chars()` but fills with `fill` instead of `Cell::EMPTY`,
    /// supporting BCE (Background Color Erase) per VT420/xterm spec.
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

        // Left boundary: if col is the continuation of a WIDE head at col-1, erasing
        // it orphans the head — clear the head too. Use the context-aware
        // `is_cell_wide_continuation` (bit 10 on cells[col] AND a WIDE head at col-1):
        // neither a lone `cells[col-1].WIDE` test (blanks a live head whose continuation
        // was overwritten by a narrow cell at col) nor a raw `cells[col].WIDE_CONTINUATION`
        // test (fires on a DECSCA-protected cell at col) is correct alone (bit-10
        // aliasing; round-7/8 fix).
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
            // The previous last-content cell (old_len - 1) is now empty — either
            // it was inside the erase range or it was the orphaned WIDE
            // continuation cleared at index `end` above (the `end >= old_len`
            // guard misses that case when the wide pair is the row's tail).
            // `old_len - 1` is in bounds since `col < old_len` implies old_len >= 1.
            self.recalculate_len_up_to(old_len);
        }
    }

    /// Selectively erase `count` cells starting at `col`, without shifting.
    ///
    /// Like [`erase_chars`](Self::erase_chars) but respects DECSCA character
    /// protection: protected cells are skipped. Per VT420/VT510, ECH honours
    /// the character protection attribute (#7523).
    ///
    /// Uses the context-aware [`is_cell_protected`](Self::is_cell_protected)
    /// check to correctly handle wide characters.
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

        // Left boundary fixup: if the first cell in the range is a
        // WIDE_CONTINUATION of an unprotected WIDE cell outside the
        // range, clearing the continuation creates an orphaned WIDE
        // cell. Clear the WIDE head too — AND the in-range continuation
        // at `col` itself. Once the head at col-1 is cleared to a non-WIDE
        // fill, is_cell_protected(col) reads the continuation's bit 10 (which
        // aliases PROTECTED) as protected, so the forward loop below would
        // SKIP col and leave a phantom protected spacer. Mirrors the sibling
        // erase primitives selective_clear_range / selective_wipe_range,
        // which clear both halves for exactly this reason (bit-10 class).
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
            // Do NOT gate on `end >= old_len`: the WIDE-head co-clear above clears
            // cells[c+1], which for c == end-1 is cells[end], ONE column past the
            // range. When that was the tail content (old_len == end + 1) the old
            // guard missed it and len stayed stale-high. recalculate_len_up_to(old_len)
            // rescans the prefix and never over-shrinks (matches selective_clear_range
            // / selective_wipe_range / clear_range).
            if col < old_len && self.cells[old_len - 1].is_empty() {
                self.recalculate_len_up_to(old_len);
            }
        }
    }
}
