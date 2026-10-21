// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character write operations for Row.
//!
//! Single-width and wide character writes with automatic fixup of orphaned
//! wide character halves.

use super::super::CellFlags;
use super::super::cell::Cell;
use super::{Row, RowFlags, u16_from_usize};

impl Row {
    /// Write a character at the given column with current style.
    ///
    /// If overwriting part of a wide character, the orphaned half is cleared to space.
    #[inline]
    pub fn write_char(&mut self, col: u16, c: char) -> bool {
        let col_usize = col as usize;
        let old_len = self.len as usize;
        let mut end_half_blanked = false;

        if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && let Some(current_flags) = self.cells.get(col_usize).map(|c| c.flags())
        {
            // Overwriting a genuine continuation orphans its head at col-1. Context-aware
            // check: bit 10 aliases PROTECTED, so a raw `contains(WIDE_CONTINUATION)` would
            // blank col-1 when overwriting a protected cell. False for col 0, so no underflow.
            if self.is_cell_wide_continuation(col)
                && let Some(prev_cell) = self.cells.get_mut((col - 1) as usize)
            {
                *prev_cell = Cell::EMPTY;
            }
            if current_flags.contains(CellFlags::WIDE)
                && let Some(next_cell) = self.cells.get_mut(col_usize + 1)
            {
                *next_cell = Cell::EMPTY;
                end_half_blanked = true;
            }
        }

        if col_usize >= self.cells.len() {
            return false;
        }
        self.cells[col_usize].set_char(c);
        self.note_write_through(col_usize + 1);
        self.settle_len_after_write(old_len, col_usize + 1, end_half_blanked);
        self.flags |= RowFlags::DIRTY;
        true
    }

    /// `len` after a write over `[.., end)`: the one rule every writer keeps.
    ///
    /// `len` is one past the last cell output has written: a printed glyph, a printed blank
    /// included, or a mark attached to a cell. The render frame stops at it, copy and search
    /// keep a soft-wrapped row's text through it (`TrailingBlanks::Written`), a history line
    /// stores that many cells, and a rewrap keeps the printed blanks it covers, so a printed
    /// blank counts however it was written.
    ///
    /// A wide half the write's fixup blanked was not written: when it was the row's last
    /// cell (`end + 1 == old_len`), `len` ends at the write rather than leave a phantom
    /// trailing space. Erasing is not writing: the erase paths shrink `len` to the last cell
    /// that still holds content.
    #[inline]
    pub(super) fn settle_len_after_write(
        &mut self,
        old_len: usize,
        end: usize,
        end_half_blanked: bool,
    ) {
        let end = end.min(self.cells.len());
        let len = if end_half_blanked && end + 1 == old_len {
            end
        } else {
            old_len.max(end)
        };
        self.len = u16_from_usize(len);
    }

    /// Write a styled character at the given column.
    ///
    /// If overwriting part of a wide character, the orphaned half is cleared to space.
    #[inline]
    pub fn write_char_styled(
        &mut self,
        col: u16,
        c: char,
        fg: super::super::PackedColor,
        bg: super::super::PackedColor,
        flags: CellFlags,
    ) -> bool {
        self.write_char_packed(col, c, Cell::convert_colors(fg, bg), flags)
    }

    /// Write a styled character at the given column with pre-computed colors, converted
    /// once by the caller rather than per character.
    #[inline]
    pub fn write_char_packed(
        &mut self,
        col: u16,
        c: char,
        colors: super::super::PackedColors,
        flags: CellFlags,
    ) -> bool {
        let col_usize = col as usize;
        let cells_len = self.cells.len();
        let old_len = self.len as usize;

        if col_usize >= cells_len {
            return false;
        }

        // Rare path: an ASCII-only row skips the cell's flag read.
        let mut end_half_blanked = false;
        if self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            // SAFETY: col_usize < cells_len verified above
            let current_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
            let wide_mask = CellFlags::WIDE.union(CellFlags::WIDE_CONTINUATION);
            if (current_flags.bits() & wide_mask.bits()) != 0 {
                end_half_blanked =
                    self.fixup_wide_char_overwrite(col_usize, current_flags, cells_len);
            }
        }

        let cp = c as u32;
        let char_data = if cp <= Cell::MAX_DIRECT_CODEPOINT {
            cp as u16
        } else {
            '\u{FFFD}' as u16
        };

        // SAFETY: col_usize < cells_len verified above
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags);
        }
        self.note_write_through(col_usize + 1);
        self.settle_len_after_write(old_len, col_usize + 1, end_half_blanked);
        self.flags |= RowFlags::DIRTY;
        true
    }

    /// Orphan fixup for a single-cell write at `col_usize`. Returns whether it blanked the
    /// continuation after `col_usize` (the write's end half).
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_overwrite(
        &mut self,
        col_usize: usize,
        current_flags: CellFlags,
        cells_len: usize,
    ) -> bool {
        // Overwriting a genuine continuation clears its head. Context-aware check (bit 10
        // aliases PROTECTED); col_usize is written after this fixup, so it reads pre-write state.
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        // Overwriting a head clears its continuation; WIDE (bit 9) is unaliased.
        if current_flags.contains(CellFlags::WIDE) && col_usize + 1 < cells_len {
            self.cells[col_usize + 1] = Cell::EMPTY;
            return true;
        }
        false
    }

    /// Fill `count` cells from `col` with `template`: the bulk path for runs of one character
    /// (REP, padding, separator lines), a `slice::fill` the compiler lowers to a memset-like
    /// loop.
    ///
    /// Fixes up wide-character orphans at the range boundaries. Returns the number of cells
    /// written (fewer than `count` when the range passes the row end).
    #[inline]
    pub fn fill_cell_run(&mut self, col: u16, count: u16, template: Cell) -> u16 {
        if count == 0 {
            return 0;
        }

        let start = usize::from(col);
        let cells_len = self.cells.len();
        if start >= cells_len {
            return 0;
        }
        let old_len = self.len as usize;

        // Wide character fixup before overwriting the range (gated inline so the
        // REP/padding path stays free of the cold call).
        let end_half_blanked = self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && self.fixup_wide_chars_in_range(col, count);

        let end = start.saturating_add(usize::from(count)).min(cells_len);
        let actual = end - start;

        self.cells[start..end].fill(template);
        self.note_write_through(end);

        // Filled blanks are written cells, as every writer counts them.
        self.settle_len_after_write(old_len, end, end_half_blanked);
        self.flags |= RowFlags::DIRTY;

        u16_from_usize(actual)
    }

    /// The bulk counterpart of `fixup_wide_char_overwrite`, called before writing
    /// single-width cells over `[start_col, start_col + count)`: clears the head left of a
    /// continuation at the start and the continuation right of a head at the end.
    ///
    /// Returns whether it blanked the continuation at the span's end (the end half
    /// [`Self::settle_len_after_write`] takes).
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_chars_in_range(&mut self, start_col: u16, count: u16) -> bool {
        // Fast path: an ASCII-only row has nothing to fix.
        if !self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            return false;
        }
        // The cells inside the span are overwritten by the caller; only the
        // halves outside it need clearing. Each is cleared after both are read:
        // they lie on opposite sides of a non-empty span.
        let halves = self.orphaned_wide_halves(start_col, start_col.saturating_add(count));
        for col in halves.into_iter().flatten() {
            self.cells[usize::from(col)] = Cell::EMPTY;
        }
        halves[1].is_some()
    }

    /// The wide halves outside `[start, end)` that writing the span orphans: the head at
    /// `start - 1` when `start` is its continuation, and the continuation at `end` when
    /// `end - 1` is its head (`end` clipped to the row). These are the cells
    /// [`Self::fixup_wide_chars_in_range`] blanks, and for a one- or two-cell span
    /// [`Self::fixup_wide_char_overwrite`] and [`Self::fixup_wide_char_write`]; the grid's
    /// writers probe them first so a blanked half's extras entry goes with it
    /// (`grid::write::orphan_halves_with_extras`).
    ///
    /// The continuation test is context-aware (bit 10 aliases PROTECTED); WIDE (bit 9) is
    /// unaliased.
    #[must_use]
    #[inline]
    pub(crate) fn orphaned_wide_halves(&self, start: u16, end: u16) -> [Option<u16>; 2] {
        if !self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            return [None, None];
        }
        let cells_len = self.cells.len();
        let end = usize::from(end).min(cells_len);
        if usize::from(start) >= end {
            return [None, None];
        }
        // `is_cell_wide_continuation` is false at column 0, so `start - 1`
        // cannot underflow.
        let head = self
            .is_cell_wide_continuation(start)
            .then(|| start.saturating_sub(1));
        let continuation = (end < cells_len
            && self.cells[end - 1].flags().contains(CellFlags::WIDE))
        .then(|| u16_from_usize(end));
        [head, continuation]
    }

    /// Whether any cell in `[start, end)` carries `HAS_EXTRAS`.
    #[must_use]
    #[inline]
    pub(crate) fn any_has_extras_in_range(&self, start: u16, end: u16) -> bool {
        let cells = self.cells.as_slice();
        let s = usize::from(start).min(cells.len());
        let e = usize::from(end).min(cells.len());
        s < e && cells[s..e].iter().any(Cell::has_extras)
    }

    /// Write a wide (double-width) character at the given column.
    ///
    /// Returns `false` when the pair does not fit.
    #[inline]
    #[cfg(test)]
    pub fn write_wide_char(
        &mut self,
        col: u16,
        c: char,
        fg: super::super::PackedColor,
        bg: super::super::PackedColor,
        flags: CellFlags,
    ) -> bool {
        self.write_wide_char_packed(col, c, Cell::convert_colors(fg, bg), flags)
    }

    /// Write a wide character with pre-computed packed colors.
    #[inline]
    pub fn write_wide_char_packed(
        &mut self,
        col: u16,
        c: char,
        colors: super::super::PackedColors,
        flags: CellFlags,
    ) -> bool {
        let cells_len = self.cells.len();
        let col_usize = col as usize;
        let old_len = self.len as usize;

        if col_usize + 1 >= cells_len {
            return false;
        }

        // SAFETY: col_usize < cells_len and col_usize + 1 < cells_len verified above
        let first_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
        let second_flags = unsafe { self.cells.get_unchecked(col_usize + 1) }.flags();

        let end_half_blanked = (first_flags.contains(CellFlags::WIDE_CONTINUATION)
            || second_flags.contains(CellFlags::WIDE))
            && self.fixup_wide_char_write(col_usize, second_flags, cells_len);

        let cp = c as u32;
        let char_data = if cp <= Cell::MAX_DIRECT_CODEPOINT {
            cp as u16
        } else {
            '\u{FFFD}' as u16
        };

        // SAFETY: bounds already verified
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags.union(CellFlags::WIDE));
            // The spacer carries the lead's rendition (see `CellFlags::wide_continuation_of`).
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }

        self.note_write_through(col_usize + 2);
        self.settle_len_after_write(old_len, col_usize + 2, end_half_blanked);
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_WIDE_CHARS;
        true
    }

    /// Write a wide character without bounds checks or orphan fixup: the unchecked
    /// counterpart to [`Self::write_wide_char_packed`] for the emoji/CJK batch-run paths.
    ///
    /// # Safety
    ///
    /// (a) `(col as usize) + 1 < self.cells_len()`: both `col` and `col + 1` are valid indices.
    ///
    /// (b) `col` is on a wide-cell boundary (sequential writes at `col, col+2, ...` on a freshly
    ///     sized run); an odd offset inside an existing pair leaves a torn pair.
    ///
    /// (c) Writes are sequential and non-overlapping, so nothing in the row conflicts with the
    ///     skipped orphan fixup.
    #[inline]
    pub unsafe fn write_wide_char_packed_no_fixup(
        &mut self,
        col: u16,
        c: char,
        colors: super::super::PackedColors,
        flags: CellFlags,
    ) {
        let col_usize = col as usize;
        debug_assert!(
            col_usize + 1 < self.cells.len(),
            "write_wide_char_packed_no_fixup: col + 1 ({}) must be < cells.len() ({})",
            col_usize + 1,
            self.cells.len()
        );
        let cp = c as u32;
        let char_data = if cp <= Cell::MAX_DIRECT_CODEPOINT {
            cp as u16
        } else {
            '\u{FFFD}' as u16
        };

        // SAFETY: `col + 1 < self.cells.len()` is the caller's invariant (a); the
        // debug_assert above checks it in debug builds.
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags.union(CellFlags::WIDE));
            // As in `write_wide_char_packed`, the spacer inherits the lead's rendition.
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }
    }

    /// Orphan fixup for a wide-char write at `col_usize`. Returns whether it blanked the
    /// continuation after the written pair (its end half).
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_write(
        &mut self,
        col_usize: usize,
        second_flags: CellFlags,
        cells_len: usize,
    ) -> bool {
        // The first cell overwrites a genuine continuation: clear its head. Context-aware check
        // (bit 10 aliases PROTECTED), false for col 0; the cells are written after this fixup.
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        // The second cell overwrites a head: clear its continuation. WIDE (bit 9) is unaliased.
        if second_flags.contains(CellFlags::WIDE) && col_usize + 2 < cells_len {
            self.cells[col_usize + 2] = Cell::EMPTY;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::PackedColor;
    use super::super::super::cell::Cell;
    use super::super::super::cell_flags::CellFlags;
    use super::super::super::page::PageStore;
    use super::super::{Row, RowFlags};

    /// Shared test helper: create a Row with the given column count.
    fn make_row(cols: u16) -> (PageStore, Row) {
        let mut pages = PageStore::new();
        // SAFETY: Test rows never outlive their local `pages` owner.
        let row = unsafe { Row::new(cols, &mut pages) };
        (pages, row)
    }

    fn default_colors() -> (PackedColor, PackedColor) {
        (PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG)
    }

    // ── write_char ───────────────────────────────────────────────────

    #[test]
    fn write_char_out_of_bounds_returns_false() {
        let (_pages, mut row) = make_row(10);
        assert!(!row.write_char(10, 'X'));
        assert!(!row.write_char(100, 'X'));
        assert_eq!(row.len(), 0);
    }

    // ── write_char: overwrite existing content ──────────────────────

    #[test]
    fn write_char_overwrite_wide_continuation_clears_main() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();

        row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert!(row.get(2).unwrap().is_wide());
        assert!(row.get(3).unwrap().is_wide_continuation());

        row.write_char(3, 'Y');
        assert_eq!(row.get(3).unwrap().char(), 'Y');
        assert_eq!(row.get(2).unwrap().char(), ' ');
        assert!(!row.get(2).unwrap().is_wide());
    }

    // ── write_char_packed ───────────────────────────────────────────

    #[test]
    fn write_char_packed_out_of_bounds() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(5);
        let colors = PackedColors::DEFAULT;
        assert!(!row.write_char_packed(5, 'X', colors, CellFlags::empty()));
        assert!(!row.write_char_packed(100, 'X', colors, CellFlags::empty()));
        assert_eq!(row.len(), 0);
    }

    // ── write_wide_char ─────────────────────────────────────────────

    #[test]
    fn write_wide_char_sets_has_wide_chars_flag() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();
        assert!(!row.flags().contains(RowFlags::HAS_WIDE_CHARS));
        row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert!(row.flags().contains(RowFlags::HAS_WIDE_CHARS));
    }

    #[test]
    fn write_wide_char_overwrite_existing_wide_orphan_cleanup() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();

        row.write_wide_char(2, '\u{4E00}', fg, bg, CellFlags::empty());
        row.write_wide_char(4, '\u{4E8C}', fg, bg, CellFlags::empty());

        // Overlaps the first pair's continuation (col 3) and the second's head (col 4).
        row.write_wide_char(3, '\u{4E09}', fg, bg, CellFlags::empty());

        // Cols 2 and 5 are the orphans; 3-4 hold the new pair.
        assert_eq!(row.get(2).unwrap().char(), ' ');
        assert!(row.get(3).unwrap().is_wide());
        assert!(row.get(4).unwrap().is_wide_continuation());
        assert_eq!(row.get(5).unwrap().char(), ' ');
    }

    #[test]
    fn write_wide_char_packed_out_of_bounds() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(5);
        let colors = PackedColors::DEFAULT;
        // The pair would need col 5, past the row.
        assert!(!row.write_wide_char_packed(4, '\u{4E2D}', colors, CellFlags::empty()));
        assert_eq!(row.len(), 0);
    }

    // ── fill_cell_run ───────────────────────────────────────────────

    /// A run of printed default blanks is written text: `len` covers it as it
    /// covers the same blanks written one at a time, so a splatted or REP'd run
    /// of spaces is part of the line.
    #[test]
    fn fill_cell_run_counts_written_blanks() {
        let (_pages, mut row) = make_row(10);
        row.write_char(0, 'A');
        assert_eq!(row.len(), 1);

        let written = row.fill_cell_run(5, 3, Cell::EMPTY);
        assert_eq!(written, 3);
        assert_eq!(row.len(), 8, "the filled blanks are written");

        let (_pages, mut per_glyph) = make_row(10);
        per_glyph.write_char(0, 'A');
        for col in 5..8 {
            per_glyph.write_char(col, ' ');
        }
        assert_eq!(row.len(), per_glyph.len(), "the same as one at a time");
    }

    // ── len tracking edge cases ─────────────────────────────────────

    #[test]
    fn write_char_extends_len_for_sparse_writes() {
        let (_pages, mut row) = make_row(80);
        row.write_char(50, 'A');
        assert_eq!(row.len(), 51);
        row.write_char(10, 'B');
        assert_eq!(row.len(), 51);
    }
}
