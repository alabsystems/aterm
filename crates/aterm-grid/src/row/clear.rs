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

    /// Erase all cell content but preserve DEC line attributes (DECDWL/DECDHL).
    ///
    /// Per VT420/VT510 spec and xterm, erase operations (ED/EL) clear
    /// character positions but do not change line attributes. Use this
    /// instead of `clear()` in erase code paths (#7497).
    #[inline]
    pub fn erase(&mut self) {
        self.cells.fill(Cell::EMPTY);
        self.len = 0;
        self.flags = (self.flags & RowFlags::LINE_ATTRIBUTES) | RowFlags::DIRTY;
    }

    /// Erase with BCE fill cell, preserving DEC line attributes (#7522).
    ///
    /// Like `erase()` but fills cells with `fill` instead of `Cell::EMPTY`,
    /// supporting BCE (Background Color Erase) per VT420/xterm spec.
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
    /// Semantically identical to `clear()` followed by `erase_with(fill)`
    /// (cells = `fill`, len per BCE rule, flags = DIRTY with line attributes
    /// dropped) but writes each cell exactly once. Used by the scroll path
    /// when recycling ring-buffer rows, where the old content and line
    /// attributes are always discarded.
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

            // Wide character fixup: if cells[start-1] is a WIDE base, its
            // continuation (at start) is about to be cleared, orphaning the base.
            // Key on the LEFT NEIGHBOR being WIDE (bit 9, unaliased), NOT on
            // WIDE_CONTINUATION of cells[start] — bit 10 is shared with PROTECTED, so
            // the raw check misfires on a DECSCA-protected cell at start and corrupts
            // cells[start-1], which is OUTSIDE the cleared range (round-5 DECCRA fix,
            // generalized). A WIDE base's continuation is always the next cell, so
            // checking the neighbor is exactly equivalent for genuine orphans.
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

    /// Clear cells from start to `end` (exclusive).
    #[inline]
    #[allow(dead_code, reason = "used by Kani proofs and integration tests")]
    pub(crate) fn clear_range(&mut self, start: u16, end: u16) {
        let start = start as usize;
        let cols = self.cells.len();
        let end = (end as usize).min(cols);
        if start < end {
            let old_len = self.len as usize;

            // Wide character fixup: clearing at a boundary that bisects a wide
            // character pair creates orphaned halves that must be cleared.

            // Left boundary: if cells[start-1] is a WIDE base, its continuation at
            // `start` is being cleared, orphaning the base. Key on the left neighbor
            // being WIDE (bit 9, unaliased) — NOT WIDE_CONTINUATION of cells[start],
            // whose bit 10 aliases PROTECTED and would corrupt the out-of-range
            // cells[start-1] for a protected cell (round-5 DECCRA fix, generalized).
            if start > 0 && self.cells[start - 1].flags().contains(CellFlags::WIDE) {
                self.cells[start - 1] = Cell::EMPTY;
            }

            // Right boundary: if the cell just before `end` is WIDE, its
            // continuation at `end` is not cleared and becomes orphaned.
            let mut cleared_right_orphan = false;
            if end > start && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
                self.cells[end] = Cell::EMPTY;
                cleared_right_orphan = true;
            }

            self.cells[start..end].fill(Cell::EMPTY);
            self.flags |= RowFlags::DIRTY;
            // Recalc when the clear reached the old content end, OR when the right
            // wide-orphan cleared at index `end` was the last content cell
            // (old_len == end + 1) — then [start, old_len) is fully empty and
            // recalculate_len_up_to(start) is correct. Mirrors clear_range_with
            // (#7522); without the second term len would be left stale-high.
            if start < old_len && (end >= old_len || (cleared_right_orphan && old_len == end + 1)) {
                self.recalculate_len_up_to(start);
            }
        }
    }

    /// Clear cells from start to `end` (exclusive) with a BCE fill cell (#7522).
    ///
    /// Like `clear_range()` but fills with `fill` instead of `Cell::EMPTY`.
    #[inline]
    pub(crate) fn clear_range_with(&mut self, start: u16, end: u16, fill: Cell) {
        let start = start as usize;
        let cols = self.cells.len();
        let end = (end as usize).min(cols);
        if start < end {
            let old_len = self.len as usize;

            // Left-boundary wide fixup (same as clear_range, with the fill cell): key
            // on the left neighbor being WIDE (bit 9), NOT WIDE_CONTINUATION of
            // cells[start] — bit 10 aliases PROTECTED, so the raw check corrupts the
            // out-of-range cells[start-1] for a DECSCA-protected cell (round-5 DECCRA
            // fix, generalized to this DECERA/DECFRA/EL/ED-backing helper).
            if start > 0 && self.cells[start - 1].flags().contains(CellFlags::WIDE) {
                // The orphaned WIDE head at start-1 is OUTSIDE the fill rect, so
                // it must not inherit the DECFRA/BCE fill glyph or attributes —
                // clear it to EMPTY exactly like clear_range() does. Writing
                // `fill` here bled the fill one column left of the rect (#7522).
                self.cells[start - 1] = Cell::EMPTY;
            }

            let mut cleared_right_orphan = false;
            if end > start && end < cols && self.cells[end - 1].flags().contains(CellFlags::WIDE) {
                // Same reasoning on the right: cells[end] is the orphaned
                // continuation OUTSIDE the rect and must be cleared, not filled.
                self.cells[end] = Cell::EMPTY;
                cleared_right_orphan = true;
            }

            self.cells[start..end].fill(fill);
            self.flags |= RowFlags::DIRTY;
            if !fill.is_empty() {
                // Visible fill — a BCE background, a DECFRA fill character,
                // or attribute flags: len must cover the filled range so the
                // read path (row_text/render_row) does not drop it (a DECFRA
                // fill with default colors is still content, per VT420/VT520
                // DECFRA the filled characters are displayed).
                //
                // The visible fill guarantees content through end-1, so len >= end;
                // pre-existing content past the rect extends len to old_len. The one
                // exception: when the ONLY cell past the fill was the wide-continuation
                // orphan we just cleared to EMPTY (old_len == end + 1), that cell is no
                // longer content, so len is exactly `end`. Without this, the row keeps a
                // bogus trailing cell in its logical length — row_text and scrollback
                // materialization slice through self.len, so it would surface as a stale
                // trailing space one column past the rect (#7522).
                let new_end = if cleared_right_orphan && old_len == end + 1 {
                    end
                } else {
                    end.max(old_len)
                };
                self.len = super::u16_from_usize(new_end);
            } else if start < old_len
                && (end >= old_len || (cleared_right_orphan && old_len == end + 1))
            {
                // Recalc when the fill reached the old content end, OR when the
                // right wide-orphan we cleared at index `end` was the row's last
                // content cell (old_len == end + 1). In both cases [start, old_len)
                // is now fully empty, so recalculate_len_up_to(start) yields the
                // tight len. Mirrors the visible-fill orphan case above; without the
                // second term an EL/DECERA that erases a trailing wide char left len
                // stale-high and surfaced phantom trailing spaces (#7522).
                self.recalculate_len_up_to(start);
            }
        }
    }

    /// Fix orphaned wide character halves at rectangular operation boundaries.
    ///
    /// After copying/clearing cells within columns [left, right], wide
    /// character pairs that span the boundary may have been bisected.
    /// Clears the orphaned half of any such pair (#7500).
    ///
    /// `left` and `right` are inclusive column indices. `cols` is the total
    /// number of columns in the row.
    ///
    /// `clear_orphan_spacers` controls the two AMBIGUOUS branches (an apparent
    /// orphaned continuation whose WIDE head is *not* to its left). Because
    /// PROTECTED aliases WIDE_CONTINUATION (bit 10) and both a real continuation
    /// spacer and a DECSCA-protected space carry char ' ', those branches cannot
    /// tell a genuine orphaned spacer from a protected space by inspecting the cell
    /// alone. Callers that have ALREADY cleaned genuine boundary orphans
    /// authoritatively (the DECLRMM column shifts — DECIC/DECDC/SL/SR/DECBI/DECFI —
    /// run `insert/delete_chars_bounded_fill`, which fixes the margins via the
    /// context-aware `is_cell_wide_continuation`) pass `false`: for them any
    /// remaining bit-10 cell at the boundary can only be a protected space, so
    /// clearing it would corrupt content OUTSIDE the operated region (codex round-7
    /// finding — SL blanked a protected space one column past the right margin).
    /// Whole-row rect copies/clears (vertical scroll, IL/DL) pass `true`: they have
    /// no per-cell authoritative signal, so they keep the char==' ' heuristic
    /// (residual: a protected SPACE bisected by a vertical rect scroll is still
    /// cleared — fully closing that needs the shared bit to be un-aliased).
    ///
    /// The two UNAMBIGUOUS branches (a dangling WIDE head whose continuation is
    /// gone) always run: they key on the neighbor's WIDE bit (bit 9, unaliased) and
    /// clear the WIDE head, never a protected cell.
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
        // `next_cont && !cur_wide` branch has the same bit-10 aliasing hazard AND
        // targets cells[right+1] which is OUTSIDE the operated region [left,right].
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
    pub(crate) fn is_cell_wide_continuation(&self, col: u16) -> bool {
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

            // Left boundary fixup: if the first cell in the range is a
            // WIDE_CONTINUATION of an unprotected WIDE cell outside the
            // range, clearing the continuation creates an orphaned WIDE
            // cell. Clear the WIDE cell too (#7462).
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
                // Clearing the lead's WIDE flag would make the in-range
                // continuation at `start_usize` read as a standalone PROTECTED
                // cell (WIDE_CONTINUATION and PROTECTED share bit 10), so the
                // loop below would skip it and orphan the spacer. Clear it here.
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
                // Do NOT gate on `end_usize >= old_len`: the WIDE-head co-clear
                // above wipes cells[col+1], which for col == end-1 is cells[end],
                // ONE column past the range. When that was the tail content
                // (old_len == end + 1), `end_usize >= old_len` is false and len
                // would be left stale-high. recalculate_len_up_to(old_len) rescans
                // the whole prefix and never over-shrinks, so keying only on the
                // old tail now being empty is correct (matches selective_clear).
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
    /// attributes." xterm matches (ScrnWipeRectangle writes ' ' into
    /// charData without touching the attribute/color arrays). Contrast with
    /// [`Row::selective_clear_range`], which DECSED/DECSEL use and which
    /// resets the cells entirely (xterm ClearCells).
    ///
    /// Cells protected by DECSCA are skipped. Uses the context-aware
    /// protection check to correctly handle wide characters; a wiped wide
    /// character becomes two plain-width spaces (`set_char` clears the
    /// COMPLEX/WIDE/WIDE_CONTINUATION structural flags but leaves SGR
    /// flags, colors, and any interned style id intact).
    ///
    /// Each column actually wiped (including the wide left-boundary fixup and
    /// the WIDE-continuation co-wipe) is pushed onto `wiped`. The row has no
    /// access to the grid-level extras map, so the caller uses this set to
    /// scrub stale combining marks / orphaned complex-char strings from the
    /// wiped cells while preserving their out-of-line truecolor (DECSERA).
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

            // Left boundary fixup: wiping the WIDE_CONTINUATION of an
            // unprotected WIDE cell outside the range would orphan the WIDE
            // cell — wipe it too (mirrors selective_clear_range, #7462).
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
                // `set_char` above cleared the lead's WIDE flag. Because
                // WIDE_CONTINUATION and PROTECTED share bit 10, the in-range
                // continuation at `start_usize` would now be misread as a
                // standalone PROTECTED cell by `is_cell_protected` (its WIDE
                // parent is gone), so the loop below would SKIP it and leave an
                // orphaned continuation spacer. Clear its bit 10 here (without
                // pushing — the loop records it normally once it is no longer
                // seen as protected).
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
                // Same off-by-one as selective_clear_range: the WIDE-head co-wipe
                // touches cells[end], one column past the range, so gating on
                // `end_usize >= old_len` misses the case where that was the tail
                // content (old_len == end + 1). recalculate_len_up_to(old_len)
                // rescans the prefix and never over-shrinks; a wiped cell that kept
                // a non-default background is not is_empty(), so len still counts it.
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
