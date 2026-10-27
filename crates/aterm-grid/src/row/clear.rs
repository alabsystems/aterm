// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Whole-row, range (BCE), and DECSCA-aware selective row clears.

use super::super::cell::Cell;
use super::super::cell_flags::CellFlags;
use super::{Row, RowFlags};

impl Row {
    /// Clears the entire row.
    #[inline]
    pub fn clear(&mut self) {
        self.cells.fill(Cell::EMPTY);
        self.len = 0;
        self.flags = RowFlags::DIRTY;
    }

    /// Fills the row with `fill` (BCE) while preserving DEC line attributes.
    #[inline]
    pub fn erase_with(&mut self, fill: Cell) {
        self.cells.fill(fill);
        if fill.colors() == Cell::EMPTY.colors() {
            self.len = 0;
        } else {
            self.len = super::u16_from_usize(self.cells.len());
        }
        self.flags = (self.flags & RowFlags::LINE_ATTRIBUTES) | RowFlags::DIRTY;
    }

    /// Resets the row to `fill` and clears DEC line attributes in a single pass.
    #[inline]
    pub fn reset_with(&mut self, fill: Cell) {
        self.cells.fill(fill);
        if fill.colors() == Cell::EMPTY.colors() {
            self.len = 0;
        } else {
            self.len = super::u16_from_usize(self.cells.len());
        }
        self.flags = RowFlags::DIRTY;
    }

    /// Fills `start..end` with `fill`, blanking any orphaned wide-char half to `Cell::EMPTY`.
    #[inline]
    pub(crate) fn clear_range_with(&mut self, start: u16, end: u16, fill: Cell) {
        let _ = self.clear_range_with_orphan(start, end, fill, Cell::EMPTY);
        if !super::is_wrap_hole_blank(&fill) {
            self.note_write_through(usize::from(end));
        }
    }

    /// Clears `start..end` with `fill`, replacing any orphaned wide-char half at either boundary with `orphan`.
    #[inline]
    pub(crate) fn clear_range_with_orphan(
        &mut self,
        start: u16,
        end: u16,
        fill: Cell,
        orphan: Cell,
    ) -> core::ops::Range<u16> {
        debug_assert!(
            orphan.is_empty() || orphan == fill,
            "Row::clear_range_with_orphan: orphan must be Cell::EMPTY (rect ops) \
             or the fill cell (BCE erases); no other value has an xterm analogue"
        );
        let start = start as usize;
        let cols = self.cells.len();
        let end = (end as usize).min(cols);
        if start >= end {
            return super::u16_from_usize(start)..super::u16_from_usize(start);
        }
        let old_len = self.len as usize;
        let mut written_start = start;
        let mut written_end = end;

        if start > 0 && self.cells[start - 1].flags().contains(CellFlags::WIDE) {
            self.cells[start - 1] = orphan;
            written_start = start - 1;
        }

        let mut cleared_right_orphan = false;
        if end > start && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
            self.cells[end] = orphan;
            cleared_right_orphan = true;
            written_end = end + 1;
        }

        self.cells[start..end].fill(fill);
        self.flags |= RowFlags::DIRTY;
        let right_orphan_is_content = cleared_right_orphan && !orphan.is_empty();
        if !fill.is_empty() {
            let new_end = if right_orphan_is_content {
                (end + 1).max(old_len)
            } else if cleared_right_orphan && old_len == end + 1 {
                end
            } else {
                end.max(old_len)
            };
            self.len = super::u16_from_usize(new_end);
        } else if start < old_len
            && (end >= old_len || (cleared_right_orphan && old_len == end + 1))
        {
            self.recalculate_len_up_to(start);
        }
        super::u16_from_usize(written_start)..super::u16_from_usize(written_end)
    }

    /// Before the columns `left..=right` of this row shift (a scroll, IL or DL between DECSLRM
    /// margins), turn each wide character straddling `left` or `right` into two narrow blanks
    /// that keep their renditions, so the shift splits no glyph and no half meets another
    /// glyph's half. Ghostty's `rowWillBeShifted` does this to every row a shift touches; xterm
    /// leaves the halves torn. Returns the columns of the leads it unpaired, whose combining
    /// marks the caller drops from the extras map, as Ghostty's `clearGrapheme` does.
    #[inline]
    pub(crate) fn unpair_margin_straddlers(
        &mut self,
        left: usize,
        right: usize,
    ) -> [Option<u16>; 2] {
        let mut leads = [None, None];
        if left > 0 && self.is_cell_wide_continuation(super::u16_from_usize(left)) {
            self.unpair_wide_at(left - 1);
            leads[0] = Some(super::u16_from_usize(left - 1));
        }
        if right + 1 < self.cells.len()
            && self.cells[right].flags().contains(CellFlags::WIDE)
            && self.is_cell_wide_continuation(super::u16_from_usize(right + 1))
        {
            self.unpair_wide_at(right);
            leads[1] = Some(super::u16_from_usize(right));
        }
        leads
    }

    /// The wide character whose lead is at `lead` (its spacer at `lead + 1`) becomes two
    /// narrow blanks, each keeping its colours and rendition; the former spacer takes the
    /// lead's `PROTECTED` bit, which its role bit stood in for. The lead's combining marks live
    /// in the grid's extras map, which this row cannot reach: the caller drops them.
    fn unpair_wide_at(&mut self, lead: usize) {
        let mut lead_flags = self.cells[lead].flags();
        lead_flags.remove(CellFlags::WIDE);
        lead_flags.remove(CellFlags::COMPLEX);
        let protected = lead_flags.intersects(CellFlags::PROTECTED);
        let mut spacer_flags = self.cells[lead + 1].flags();
        spacer_flags.remove(CellFlags::WIDE_CONTINUATION);
        if protected {
            spacer_flags = spacer_flags.union(CellFlags::PROTECTED);
        }
        let lead_colors = self.cells[lead].colors();
        let spacer_colors = self.cells[lead + 1].colors();
        self.cells[lead] = Cell::from_raw_parts(' ' as u16, lead_colors, lead_flags);
        self.cells[lead + 1] = Cell::from_raw_parts(' ' as u16, spacer_colors, spacer_flags);
        self.flags |= RowFlags::DIRTY;
    }

    /// Clears any wide-character half orphaned by a rectangular operation on columns `left..=right`.
    #[inline]
    pub(crate) fn fixup_wide_boundary(
        &mut self,
        left: usize,
        right: usize,
        cols: usize,
        clear_orphan_spacers: bool,
    ) {
        // Left boundary: if left-1 is WIDE but left is not its continuation,
        // clear the orphaned WIDE cell.
        if left > 0 && left < self.cells.len() && left - 1 < self.cells.len() {
            let prev_wide = self.cells[left - 1].flags().contains(CellFlags::WIDE);
            let cur_cont = self.cells[left]
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION);
            if prev_wide && !cur_cont {
                self.cells[left - 1] = Cell::EMPTY;
            }
            if clear_orphan_spacers && cur_cont && !prev_wide && self.cells[left].char() == ' ' {
                self.cells[left] = Cell::EMPTY;
            }
        }
        if right < self.cells.len() && right + 1 < cols && right + 1 < self.cells.len() {
            let cur_wide = self.cells[right].flags().contains(CellFlags::WIDE);
            let next_cont = self.cells[right + 1]
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION);
            if cur_wide && !next_cont {
                self.cells[right] = Cell::EMPTY;
            }
            if clear_orphan_spacers && next_cont && !cur_wide && self.cells[right + 1].char() == ' '
            {
                self.cells[right + 1] = Cell::EMPTY;
            }
        }
    }

    /// Returns `true` if `col` is DECSCA-protected, accounting for wide-character continuations.
    #[inline]
    pub(crate) fn is_cell_protected(&self, col: u16) -> bool {
        let col = col as usize;
        if col >= self.cells.len() {
            return false;
        }
        let flags = self.cells[col].flags();

        if !flags.contains(CellFlags::PROTECTED) {
            return false;
        }

        if flags.contains(CellFlags::WIDE) {
            return true;
        }

        if col > 0 && self.cells[col - 1].flags().contains(CellFlags::WIDE) {
            return self.cells[col - 1].flags().contains(CellFlags::PROTECTED);
        }

        true
    }

    /// Returns `true` if `col` is the trailing spacer of a wide character.
    #[inline]
    pub fn is_cell_wide_continuation(&self, col: u16) -> bool {
        let col = col as usize;
        if col >= self.cells.len() {
            return false;
        }
        let flags = self.cells[col].flags();

        if !flags.contains(CellFlags::WIDE_CONTINUATION) {
            return false;
        }

        if flags.contains(CellFlags::WIDE) {
            return false;
        }

        col > 0 && self.cells[col - 1].flags().contains(CellFlags::WIDE)
    }

    /// Selectively clears unprotected cells in `start..end` to `fill` (DECSEL/DECSED), returning `true` if any cell remained protected.
    #[inline]
    pub(crate) fn selective_clear_range(
        &mut self,
        start: u16,
        end: u16,
        fill: Cell,
    ) -> core::ops::Range<u16> {
        debug_assert!(
            (self.len as usize) <= self.cells.len(),
            "Row::selective_clear_range: self.len ({}) > cells.len() ({})",
            self.len,
            self.cells.len()
        );
        let start_usize = start as usize;
        let end_usize = (end as usize).min(self.cells.len());
        let mut written = start..start;
        if start_usize < end_usize {
            let old_len = self.len as usize;
            let mut any_erased = false;
            written = super::u16_from_usize(start_usize)..super::u16_from_usize(end_usize);

            if start_usize > 0
                && self.cells[start_usize]
                    .flags()
                    .contains(CellFlags::WIDE_CONTINUATION)
                && self.cells[start_usize - 1]
                    .flags()
                    .contains(CellFlags::WIDE)
                && !self.is_cell_protected((start_usize - 1) as u16)
            {
                self.cells[start_usize - 1] = fill;
                self.cells[start_usize] = fill;
                any_erased = true;
                written.start = super::u16_from_usize(start_usize - 1);
            }

            for col in start_usize..end_usize {
                if !self.is_cell_protected(col as u16) {
                    // If this is a WIDE cell, also clear its continuation so the
                    // forward iteration doesn't see an orphaned continuation whose
                    // WIDE parent was already cleared.
                    if self.cells[col].flags().contains(CellFlags::WIDE)
                        && col + 1 < self.cells.len()
                    {
                        self.cells[col + 1] = fill;
                        if col + 1 == end_usize {
                            written.end = super::u16_from_usize(end_usize + 1);
                        }
                    }
                    self.cells[col] = fill;
                    any_erased = true;
                }
            }
            if any_erased {
                self.flags |= RowFlags::DIRTY;
                if fill.is_empty() {
                    // Not gated on `end_usize >= old_len`: the WIDE-head co-clear
                    // above wipes cells[col+1], which for col == end-1 is
                    // cells[end], one past the range; when that was the tail
                    // content (old_len == end + 1), that gate would leave len
                    // stale-high. recalculate_len_up_to(old_len) rescans the
                    // prefix and never over-shrinks.
                    if start_usize < old_len && self.cells[old_len - 1].is_empty() {
                        self.recalculate_len_up_to(old_len);
                    }
                } else {
                    self.recompute_len();
                }
            }
        }
        written
    }

    /// Selectively wipes characters in `start..end` while preserving visual attributes (DECSERA).
    #[inline]
    pub(crate) fn selective_wipe_range(&mut self, start: u16, end: u16, wiped: &mut Vec<u16>) {
        debug_assert!(
            (self.len as usize) <= self.cells.len(),
            "Row::selective_wipe_range: self.len ({}) > cells.len() ({})",
            self.len,
            self.cells.len()
        );
        let start_usize = start as usize;
        let end_usize = (end as usize).min(self.cells.len());
        if start_usize < end_usize {
            let old_len = self.len as usize;
            let mut any_erased = false;

            if start_usize > 0
                && self.cells[start_usize]
                    .flags()
                    .contains(CellFlags::WIDE_CONTINUATION)
                && self.cells[start_usize - 1]
                    .flags()
                    .contains(CellFlags::WIDE)
                && !self.is_cell_protected((start_usize - 1) as u16)
            {
                self.cells[start_usize - 1].set_char(' ');
                wiped.push((start_usize - 1) as u16);
                self.cells[start_usize].set_char(' ');
                any_erased = true;
            }

            for col in start_usize..end_usize {
                if !self.is_cell_protected(col as u16) {
                    // If this is a WIDE cell, also wipe its continuation so
                    // the forward iteration never sees an orphaned spacer.
                    if self.cells[col].flags().contains(CellFlags::WIDE)
                        && col + 1 < self.cells.len()
                    {
                        self.cells[col + 1].set_char(' ');
                        wiped.push((col + 1) as u16);
                    }
                    self.cells[col].set_char(' ');
                    wiped.push(col as u16);
                    any_erased = true;
                }
            }
            if any_erased {
                self.flags |= RowFlags::DIRTY;
                if start_usize < old_len && self.cells[old_len - 1].is_empty() {
                    self.recalculate_len_up_to(old_len);
                }
            }
        }
    }

    /// Selectively clears unprotected cells across the entire row to `fill`.
    #[inline]
    pub(crate) fn selective_clear(&mut self, fill: Cell) {
        debug_assert!(
            (self.len as usize) <= self.cells.len(),
            "Row::selective_clear: self.len ({}) > cells.len() ({})",
            self.len,
            self.cells.len()
        );
        let old_len = self.len as usize;
        let mut any_erased = false;
        let cols = self.cells.len();
        for col in 0..cols {
            if !self.is_cell_protected(col as u16) {
                // A cleared WIDE head takes its continuation with it.
                if self.cells[col].flags().contains(CellFlags::WIDE) && col + 1 < cols {
                    self.cells[col + 1] = fill;
                }
                self.cells[col] = fill;
                any_erased = true;
            }
        }
        if any_erased {
            self.flags |= RowFlags::DIRTY;
            if fill.is_empty() {
                if old_len > 0 && self.cells[old_len - 1].is_empty() {
                    self.recalculate_len_up_to(old_len);
                }
            } else {
                self.recompute_len();
            }
        }
    }
}
