// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row clear and selective erase operations.
//!
//! Handles full row clear, partial clear (from column to end), range clear,
//! and DECSCA-aware selective erase that preserves protected cells.

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

    /// Erase with BCE fill cell, preserving DEC line attributes.
    ///
    /// Like `erase()` but fills cells with `fill` instead of `Cell::EMPTY`
    /// (BCE per VT420/xterm). For partial erases and EL; a complete-row ED
    /// uses `reset_with()`, the same pass with line attributes dropped.
    #[inline]
    pub fn erase_with(&mut self, fill: Cell) {
        self.cells.fill(fill);
        // BCE fill cells are not "content" for len tracking.
        // If fill has a non-default bg, cells are still conceptually blank
        // (space char) but we set len to cols so renderers draw the bg.
        if fill.colors() == Cell::EMPTY.colors() {
            self.len = 0;
        } else {
            self.len = super::u16_from_usize(self.cells.len());
        }
        self.flags = (self.flags & RowFlags::LINE_ATTRIBUTES) | RowFlags::DIRTY;
    }

    /// Fully reset the row with a BCE fill cell in a single pass.
    ///
    /// Equivalent to `clear()` then `erase_with(fill)` (cells = `fill`, len per
    /// the BCE rule, flags = DIRTY with line attributes dropped) but writes
    /// each cell once. Used when the scroll path recycles ring rows and by the
    /// ED paths erasing a complete row (xterm's `ClearBufRows`: "clearing the
    /// whole row resets the doublesize characters").
    #[inline]
    pub fn reset_with(&mut self, fill: Cell) {
        self.cells.fill(fill);
        // BCE fill cells are not "content" for len tracking (see erase_with).
        if fill.colors() == Cell::EMPTY.colors() {
            self.len = 0;
        } else {
            self.len = super::u16_from_usize(self.cells.len());
        }
        self.flags = RowFlags::DIRTY;
    }

    /// Clear cells from `start` to end of row.
    #[cfg(test)]
    #[inline]
    pub(crate) fn clear_from(&mut self, start: u16) {
        let start_usize = usize::from(start);
        if start_usize < self.cells.len() {
            let old_len = self.len as usize;

            // Wide fixup: if cells[start-1] is a WIDE base, clearing its
            // continuation at start would orphan it. Key on the left neighbour's
            // WIDE bit (bit 9, unaliased), not WIDE_CONTINUATION at start: bit 10
            // is shared with PROTECTED, so the raw check would misfire on a
            // protected cell and corrupt cells[start-1], outside the range.
            if start_usize > 0
                && self.cells[start_usize - 1]
                    .flags()
                    .contains(CellFlags::WIDE)
            {
                self.cells[start_usize - 1] = Cell::EMPTY;
            }

            self.cells[start_usize..].fill(Cell::EMPTY);
            if start_usize < old_len {
                self.recalculate_len_up_to(start_usize);
            }
            self.flags |= RowFlags::DIRTY;
        }
    }

    /// Clear cells from start to `end` (exclusive) with a BCE fill cell.
    ///
    /// Fills with `fill` and blanks any orphaned wide half at either boundary
    /// to `Cell::EMPTY` (`fill = Cell::EMPTY` is the plain clear). That is the
    /// rectangle-op rule (DECERA/DECFRA); erases following xterm's `ClearRight`
    /// give the orphan the BCE blank via [`Row::clear_range_with_orphan`]. See
    /// its docs for why the two differ.
    #[inline]
    pub(crate) fn clear_range_with(&mut self, start: u16, end: u16, fill: Cell) {
        let _ = self.clear_range_with_orphan(start, end, fill, Cell::EMPTY);
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
    /// - EL / ED reach `util.c` `ClearRight`, whose wide fixup sends both orphan
    ///   spans through `ClearInLine2` -> `ClearCells` (`screen.c`), like the
    ///   erased span: `' '` with `ld->color[col] = xtermColorPair(xw)`, i.e.
    ///   the BCE background. These callers pass `orphan = fill`. (ECH follows the
    ///   same rule, but `Grid::erase_chars` uses `Row::erase_chars_with`, which
    ///   writes `fill` into both orphans itself.)
    /// - DECERA / DECFRA reach `screen.c` `ScrnFillRectangle`, whose fixup is
    ///   `Clear1Cell(ld, left - 1)` / `Clear1Cell(ld, right + 1)`: a bare clear
    ///   without fill character or colour. The orphan is outside the rectangle,
    ///   and a DECFRA fill glyph there would bleed past the rect. These callers
    ///   pass `orphan = Cell::EMPTY`, as plain `clear_range_with` does.
    ///
    /// The selective erases (DECSEL / DECSED) use `selective_clear_range`, which
    /// never reaches here and writes `Cell::EMPTY` orphans without BCE, unlike
    /// xterm (whose `ClearInLine2` clears each unprotected segment through
    /// `ClearCells`).
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
            // Same reasoning on the right: cells[end] is the orphaned
            // continuation OUTSIDE the written range.
            self.cells[end] = orphan;
            cleared_right_orphan = true;
            written_end = end + 1;
        }

        self.cells[start..end].fill(fill);
        self.flags |= RowFlags::DIRTY;
        // Is the right-hand orphan we just rewrote CONTENT? It is when the BCE
        // blank carries a visible background; it is not when it is Cell::EMPTY.
        let right_orphan_is_content = cleared_right_orphan && !orphan.is_empty();
        if !fill.is_empty() {
            // Visible fill (a BCE background, a DECFRA fill character, or
            // attribute flags): len must cover the filled range so row_text /
            // render_row keep it (DECFRA characters are displayed per VT420/VT520
            // even with default colors).
            //
            // The fill guarantees content through end-1, so len >= end; content
            // past the rect extends len to old_len. Exception: when the only cell
            // past the fill was the wide-continuation orphan just cleared to EMPTY
            // (old_len == end + 1), len is exactly `end`, or a stale trailing
            // space would appear one column past the rect. When the orphan got
            // the visible BCE blank instead, it is content, and len covers it.
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

    /// Fix orphaned wide character halves at rectangular operation boundaries.
    ///
    /// After copying/clearing cells within columns [left, right], wide
    /// character pairs spanning the boundary may have been bisected; clear the
    /// orphaned half of any such pair.
    ///
    /// `left` and `right` are inclusive column indices. `cols` is the total
    /// number of columns in the row.
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

    /// Context-aware protection check that disambiguates the shared
    /// `PROTECTED` / `WIDE_CONTINUATION` bit using neighbor information.
    ///
    /// `Cell::is_protected()` is unreliable for wide characters because
    /// `PROTECTED` and `WIDE_CONTINUATION` share bit 10. This method
    /// checks the previous cell to distinguish the two cases.
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

    /// Context-aware wide-continuation check that disambiguates the shared
    /// `WIDE_CONTINUATION` / `PROTECTED` bit using neighbor information.
    ///
    /// `Cell::is_wide_continuation()` is unreliable for protected cells
    /// because `PROTECTED` and `WIDE_CONTINUATION` share bit 10. A true
    /// continuation spacer always immediately follows its `WIDE` main cell;
    /// a bit-10 cell without that neighbor is a DECSCA-protected cell.
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

    /// Selectively clear cells from start to `end` (exclusive).
    ///
    /// Only erases cells that are NOT protected (DECSCA).
    /// Protected cells are skipped. Uses context-aware protection
    /// check to correctly handle wide characters.
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

    /// Selectively wipe characters from `start` to `end` (exclusive),
    /// preserving visual attributes (DECSERA semantics).
    ///
    /// Per VT520 (EK-VT520-RM, DECSERA): erased positions become spaces, but
    /// "DECSERA does not change: visual attributes set by the select graphic
    /// rendition (SGR) function; protection attributes set by DECSCA; line
    /// attributes." xterm matches (ScrnWipeRectangle writes ' ' into charData
    /// only). Contrast [`Row::selective_clear_range`] (DECSED/DECSEL), which
    /// resets cells entirely (xterm ClearCells).
    ///
    /// Protected cells are skipped, using the context-aware protection check;
    /// a wiped wide character becomes two narrow spaces (`set_char` clears
    /// COMPLEX/WIDE/WIDE_CONTINUATION but keeps SGR flags, colors and any
    /// interned style id).
    ///
    /// Each wiped column (including the wide left-boundary fixup and the
    /// continuation co-wipe) is pushed onto `wiped`, so the caller can scrub
    /// stale combining marks / complex-char strings from the grid extras while
    /// keeping their truecolor.
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

    /// Selectively clear the entire row.
    ///
    /// Only erases cells that are NOT protected (DECSCA).
    /// Protected cells are skipped. Uses context-aware protection
    /// check to correctly handle wide characters.
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
                // If this is a WIDE cell, also clear its continuation so the
                // forward iteration doesn't see an orphaned continuation whose
                // WIDE parent was already cleared.
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
