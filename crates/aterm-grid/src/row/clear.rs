// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row clears: whole, range (BCE), and DECSCA-aware selective erase that
//! preserves protected cells.

use super::super::cell::Cell;
use super::super::cell_flags::CellFlags;
use super::{Row, RowFlags};

impl Row {
    /// Clear the entire row.
    #[inline]
    pub fn clear(&mut self) {
        self.cells.fill(Cell::EMPTY);
        self.len = 0;
        self.flags = RowFlags::DIRTY;
    }

    /// Fill the whole row with `fill` (BCE per VT420/xterm), keeping DEC line
    /// attributes: EL 2, and the rows IL/DL and scrolls vacate. A complete-row
    /// ED uses `reset_with()`, which drops them.
    #[inline]
    pub fn erase_with(&mut self, fill: Cell) {
        self.cells.fill(fill);
        // A default-coloured fill leaves the row empty; a BCE background
        // counts as content so renderers draw it.
        if fill.colors() == Cell::EMPTY.colors() {
            self.len = 0;
        } else {
            self.len = super::u16_from_usize(self.cells.len());
        }
        self.flags = (self.flags & RowFlags::LINE_ATTRIBUTES) | RowFlags::DIRTY;
    }

    /// `clear()` then `erase_with(fill)` in one pass: cells = `fill`, len per
    /// the BCE rule, flags = DIRTY with line attributes dropped. For ring rows
    /// the scroll path recycles and ED erasing a complete row (xterm's
    /// `ClearBufRows` resets double-size lines).
    #[inline]
    pub fn reset_with(&mut self, fill: Cell) {
        self.cells.fill(fill);
        // The len rule of `erase_with`.
        if fill.colors() == Cell::EMPTY.colors() {
            self.len = 0;
        } else {
            self.len = super::u16_from_usize(self.cells.len());
        }
        self.flags = RowFlags::DIRTY;
    }

    /// Fill `start..end` with `fill` (`Cell::EMPTY` is the plain clear),
    /// blanking an orphaned wide half at either boundary to `Cell::EMPTY`: the
    /// rectangle-op rule (DECERA/DECFRA). See [`Row::clear_range_with_orphan`]
    /// for the erases that give the orphan the BCE blank.
    #[inline]
    pub(crate) fn clear_range_with(&mut self, start: u16, end: u16, fill: Cell) {
        let _ = self.clear_range_with_orphan(start, end, fill, Cell::EMPTY);
        // A DECFRA glyph is content: one written into an early-wrap hole ends
        // it. A blank fill (DECERA) erases, and the hole stays a hole.
        if !super::is_wrap_hole_blank(&fill) {
            self.note_write_through(usize::from(end));
        }
    }

    /// Clear cells from start to `end` (exclusive) with `fill`, giving an
    /// orphaned wide half at either boundary the cell `orphan`.
    ///
    /// Returns the column range actually written: `[start, end)` widened by one
    /// on each side where a wide pair was bisected and its orphan rewritten, so
    /// callers can keep the truecolor side tables in step (see
    /// `Grid::fill_bce_rgb_range`). "Written", not "holding `fill`": with
    /// `orphan = Cell::EMPTY` the widened columns are blank and outside the
    /// rectangle.
    ///
    /// # Why the orphan cell is a parameter
    ///
    /// xterm rewrites a bisected pair's surviving half differently by operation:
    ///
    /// - EL / ED reach `util.c` `ClearRight`, whose fixup sends both orphans
    ///   through `ClearInLine2` -> `ClearCells` like the erased span: `' '` in
    ///   the BCE background. These callers pass `orphan = fill`. (ECH follows
    ///   the same rule through `Row::erase_chars_with`, which fills both
    ///   orphans itself.)
    /// - DECERA / DECFRA reach `screen.c` `ScrnFillRectangle`, whose fixup is a
    ///   bare `Clear1Cell` without fill character or colour: the orphan is
    ///   outside the rectangle, where a DECFRA fill glyph would bleed past it.
    ///   These callers pass `orphan = Cell::EMPTY`.
    ///
    /// The selective erases (DECSEL / DECSED) use `selective_clear_range`,
    /// which writes `Cell::EMPTY` orphans without BCE, unlike xterm.
    ///
    /// `orphan` is therefore `Cell::EMPTY` or `fill`; the `len` bookkeeping
    /// below relies on that (a blank fill implies a blank orphan).
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

        // Left-boundary wide fixup: key on the left neighbour's WIDE bit (bit 9),
        // not WIDE_CONTINUATION at start (bit 10 aliases PROTECTED, so the raw
        // check would corrupt cells[start-1] next to a protected cell).
        if start > 0 && self.cells[start - 1].flags().contains(CellFlags::WIDE) {
            // The orphaned WIDE head at start-1 is outside the written range, so
            // it gets `orphan`, never `fill`: EMPTY for the rect ops (a DECFRA
            // glyph there would bleed left), the BCE blank for ClearRight erases.
            self.cells[start - 1] = orphan;
            written_start = start - 1;
        }

        let mut cleared_right_orphan = false;
        if end > start && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
            // Likewise on the right: cells[end] is the orphaned continuation,
            // outside the written range.
            self.cells[end] = orphan;
            cleared_right_orphan = true;
            written_end = end + 1;
        }

        self.cells[start..end].fill(fill);
        self.flags |= RowFlags::DIRTY;
        // The right orphan is content when it got the BCE blank, not when it
        // got Cell::EMPTY.
        let right_orphan_is_content = cleared_right_orphan && !orphan.is_empty();
        if !fill.is_empty() {
            // A visible fill (BCE background, DECFRA glyph, attribute flags)
            // is content through end-1; DECFRA glyphs display even with
            // default colours (VT420/VT520). Content past the rect keeps
            // old_len, except when the only cell past it was the continuation
            // just cleared to EMPTY (old_len == end + 1): len is then `end`,
            // or a stale trailing space shows one column past the rect.
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
            // Recalculate when the fill reached the old content end, or when the
            // right wide orphan cleared at `end` was the row's last content cell
            // (old_len == end + 1): [start, old_len) is then empty, so
            // recalculate_len_up_to(start) gives the tight len. Otherwise an
            // EL/DECERA erasing a trailing wide char would leave len stale-high.
            //
            // A blank `fill` implies a blank `orphan` (see the debug_assert), so
            // there is no visible-orphan case here.
            self.recalculate_len_up_to(start);
        }
        super::u16_from_usize(written_start)..super::u16_from_usize(written_end)
    }

    /// Clear the orphaned half of any wide pair a rectangular copy/clear of
    /// columns `[left, right]` (inclusive) bisected; `cols` is the row width.
    ///
    /// `clear_orphan_spacers` controls the two ambiguous branches (an apparent
    /// orphaned continuation whose WIDE head is not to its left). PROTECTED
    /// aliases WIDE_CONTINUATION (bit 10) and both a real spacer and a
    /// protected space hold ' ', so the cell alone cannot tell them apart.
    /// Callers that already fixed genuine boundary orphans (the DECLRMM column
    /// shifts DECIC/DECDC/SL/SR/DECBI/DECFI, via
    /// `insert/delete_chars_bounded_fill` and `is_cell_wide_continuation`)
    /// pass `false`: any remaining bit-10 boundary cell is a protected space,
    /// and clearing it would corrupt content outside the region. Whole-row rect
    /// copies/clears (vertical scroll, IL/DL) have no such signal and pass
    /// `true`, keeping the char==' ' heuristic (a protected space bisected by a
    /// vertical rect scroll is still cleared).
    ///
    /// The two unambiguous branches (a WIDE head whose continuation is gone)
    /// always run: they key on the WIDE bit (bit 9, unaliased) and clear the
    /// head, never a protected cell.
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
        // Right boundary: fix orphaned pairs at right/right+1. The
        // `next_cont && !cur_wide` branch has the same bit-10 aliasing hazard and
        // targets cells[right+1], outside the region [left, right].
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

    /// Whether `col` is DECSCA-protected. `Cell::is_protected()` cannot tell:
    /// WIDE_CONTINUATION shares bit 10, so the previous cell decides.
    #[inline]
    pub(crate) fn is_cell_protected(&self, col: u16) -> bool {
        let col = col as usize;
        if col >= self.cells.len() {
            return false;
        }
        let flags = self.cells[col].flags();

        // Bit 10 not set → definitely not protected
        if !flags.contains(CellFlags::PROTECTED) {
            return false;
        }

        // Bit 9 (WIDE) set → wide main cell, bit 10 = PROTECTED
        if flags.contains(CellFlags::WIDE) {
            return true;
        }

        // Bit 10 set, bit 9 clear → PROTECTED or WIDE_CONTINUATION?
        // Check if previous cell is a wide main cell → this is a continuation.
        // A continuation cell inherits protection from its WIDE parent.
        if col > 0 && self.cells[col - 1].flags().contains(CellFlags::WIDE) {
            // This is a continuation cell. It is protected iff the WIDE cell is.
            return self.cells[col - 1].flags().contains(CellFlags::PROTECTED);
        }

        true // normal protected cell
    }

    /// Whether `col` is a wide continuation. `Cell::is_wide_continuation()`
    /// cannot tell: PROTECTED shares bit 10. A real spacer immediately follows
    /// its WIDE head; a bit-10 cell without one is DECSCA-protected.
    #[inline]
    pub fn is_cell_wide_continuation(&self, col: u16) -> bool {
        let col = col as usize;
        if col >= self.cells.len() {
            return false;
        }
        let flags = self.cells[col].flags();

        // Bit 10 not set → definitely not a continuation
        if !flags.contains(CellFlags::WIDE_CONTINUATION) {
            return false;
        }

        // Bit 9 (WIDE) set → wide main cell, bit 10 = PROTECTED
        if flags.contains(CellFlags::WIDE) {
            return false;
        }

        // Continuation iff the previous cell is the wide main cell.
        col > 0 && self.cells[col - 1].flags().contains(CellFlags::WIDE)
    }

    /// Selectively clear cells from `start` to `end` (exclusive), skipping
    /// DECSCA-protected cells (context-aware, so wide pairs are handled).
    #[inline]
    pub(crate) fn selective_clear_range(&mut self, start: u16, end: u16) {
        debug_assert!(
            (self.len as usize) <= self.cells.len(),
            "Row::selective_clear_range: self.len ({}) > cells.len() ({})",
            self.len,
            self.cells.len()
        );
        let start_usize = start as usize;
        let end_usize = (end as usize).min(self.cells.len());
        if start_usize < end_usize {
            let old_len = self.len as usize;
            let mut any_erased = false;

            // Left boundary fixup: if the first cell is the continuation of an
            // unprotected WIDE cell outside the range, clear the WIDE cell too
            // so it is not orphaned.
            if start_usize > 0
                && self.cells[start_usize]
                    .flags()
                    .contains(CellFlags::WIDE_CONTINUATION)
                && self.cells[start_usize - 1]
                    .flags()
                    .contains(CellFlags::WIDE)
                && !self.is_cell_protected((start_usize - 1) as u16)
            {
                self.cells[start_usize - 1] = Cell::EMPTY;
                // Clearing the lead's WIDE flag would make the continuation at
                // `start_usize` read as a standalone PROTECTED cell (shared bit
                // 10), so the loop would skip it and orphan the spacer; clear it
                // here.
                self.cells[start_usize] = Cell::EMPTY;
                any_erased = true;
            }

            for col in start_usize..end_usize {
                if !self.is_cell_protected(col as u16) {
                    // If this is a WIDE cell, also clear its continuation so the
                    // forward iteration doesn't see an orphaned continuation whose
                    // WIDE parent was already cleared.
                    if self.cells[col].flags().contains(CellFlags::WIDE)
                        && col + 1 < self.cells.len()
                    {
                        self.cells[col + 1] = Cell::EMPTY;
                    }
                    self.cells[col] = Cell::EMPTY;
                    any_erased = true;
                }
            }
            if any_erased {
                self.flags |= RowFlags::DIRTY;
                // Not gated on `end_usize >= old_len`: the WIDE-head co-clear
                // above wipes cells[col+1], which for col == end-1 is cells[end],
                // one past the range; when that was the tail content
                // (old_len == end + 1), that gate would leave len stale-high.
                // recalculate_len_up_to(old_len) rescans the prefix and never
                // over-shrinks.
                if start_usize < old_len && self.cells[old_len - 1].is_empty() {
                    self.recalculate_len_up_to(old_len);
                }
            }
        }
    }

    /// Selectively wipe characters from `start` to `end` (exclusive), keeping
    /// visual attributes (DECSERA).
    ///
    /// Per VT520 (EK-VT520-RM), erased positions become spaces but "DECSERA does
    /// not change: visual attributes set by the select graphic rendition (SGR)
    /// function; protection attributes set by DECSCA; line attributes." xterm
    /// matches (ScrnWipeRectangle writes ' ' into charData only). Contrast
    /// [`Row::selective_clear_range`] (DECSED/DECSEL), which resets cells.
    ///
    /// Protected cells are skipped; a wiped wide character becomes two narrow
    /// spaces (`set_char` clears COMPLEX/WIDE/WIDE_CONTINUATION but keeps SGR
    /// flags, colors and any style id). Each wiped column is pushed onto
    /// `wiped`, so the caller can scrub stale combining marks and complex-char
    /// strings from the extras while keeping their truecolor.
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

            // Left boundary fixup: wiping the continuation of an unprotected WIDE
            // cell outside the range would orphan it, so wipe it too (as
            // selective_clear_range does).
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
                // `set_char` above cleared the lead's WIDE flag, so the
                // continuation at `start_usize` would now read as a standalone
                // PROTECTED cell (shared bit 10) and be skipped, leaving an
                // orphaned spacer. Clear its bit 10 here; the loop records it
                // once it no longer reads as protected.
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
                // As in selective_clear_range: the WIDE-head co-wipe touches
                // cells[end], so gating on `end_usize >= old_len` would miss that
                // tail case. recalculate_len_up_to(old_len) never over-shrinks,
                // and a wiped cell with a non-default background still counts.
                if start_usize < old_len && self.cells[old_len - 1].is_empty() {
                    self.recalculate_len_up_to(old_len);
                }
            }
        }
    }

    /// Selectively clear the whole row, skipping DECSCA-protected cells
    /// (context-aware, so wide pairs are handled).
    #[inline]
    pub(crate) fn selective_clear(&mut self) {
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
                    self.cells[col + 1] = Cell::EMPTY;
                }
                self.cells[col] = Cell::EMPTY;
                any_erased = true;
            }
        }
        if any_erased {
            self.flags |= RowFlags::DIRTY;
            if old_len > 0 && self.cells[old_len - 1].is_empty() {
                self.recalculate_len_up_to(old_len);
            }
        }
    }
}
