// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character write operations for `Row`.

use super::super::CellFlags;
use super::super::cell::Cell;
use super::{Row, RowFlags, u16_from_usize};

impl Row {
    /// Write a character at the given column with current style.
    #[inline]
    pub fn write_char(&mut self, col: u16, c: char) -> bool {
        let col_usize = col as usize;
        let old_len = self.len as usize;
        let mut end_half_blanked = false;

        if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && let Some(current_flags) = self.cells.get(col_usize).map(|c| c.flags())
        {
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

    /// Updated `len` after writing `[.., end)`.
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

    /// Write a styled character at the given column with pre-computed colors.
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

        let mut end_half_blanked = false;
        if self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            // SAFETY: Col_usize < cells_len verified above.
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

        // SAFETY: Col_usize < cells_len verified above.
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags);
        }
        self.note_write_through(col_usize + 1);
        self.settle_len_after_write(old_len, col_usize + 1, end_half_blanked);
        self.flags |= RowFlags::DIRTY;
        true
    }

    /// Orphan fixup for a single-cell write at `col_usize`.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_overwrite(
        &mut self,
        col_usize: usize,
        current_flags: CellFlags,
        cells_len: usize,
    ) -> bool {
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        if current_flags.contains(CellFlags::WIDE) && col_usize + 1 < cells_len {
            self.cells[col_usize + 1] = Cell::EMPTY;
            return true;
        }
        false
    }

    /// Fill `count` cells from `col` with `template`.
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

        let end_half_blanked = self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && self.fixup_wide_chars_in_range(col, count);

        let end = start.saturating_add(usize::from(count)).min(cells_len);
        let actual = end - start;

        self.cells[start..end].fill(template);
        self.note_write_through(end);

        self.settle_len_after_write(old_len, end, end_half_blanked);
        self.flags |= RowFlags::DIRTY;

        u16_from_usize(actual)
    }

    /// Clear orphaned wide-character halves before writing `start_col..start_col + count`.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_chars_in_range(&mut self, start_col: u16, count: u16) -> bool {
        if !self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            return false;
        }
        let halves = self.orphaned_wide_halves(start_col, start_col.saturating_add(count));
        for col in halves.into_iter().flatten() {
            self.cells[usize::from(col)] = Cell::EMPTY;
        }
        halves[1].is_some()
    }

    /// Clear wide-character halves outside `start..end` orphaned by overwriting the span.
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
        let head = self
            .is_cell_wide_continuation(start)
            .then(|| start.saturating_sub(1));
        let continuation = (end < cells_len
            && self.cells[end - 1].flags().contains(CellFlags::WIDE))
        .then(|| u16_from_usize(end));
        [head, continuation]
    }

    /// Whether any cell in `start..end` carries `HAS_EXTRAS`.
    #[must_use]
    #[inline]
    pub(crate) fn any_has_extras_in_range(&self, start: u16, end: u16) -> bool {
        let cells = self.cells.as_slice();
        let s = usize::from(start).min(cells.len());
        let e = usize::from(end).min(cells.len());
        s < e && cells[s..e].iter().any(Cell::has_extras)
    }

    /// Write a wide (double-width) character at the given column.
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

        // SAFETY: Col_usize < cells_len and col_usize + 1 < cells_len verified above.
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

        // SAFETY: Bounds already verified.
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags.union(CellFlags::WIDE));
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }

        self.note_write_through(col_usize + 2);
        self.settle_len_after_write(old_len, col_usize + 2, end_half_blanked);
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_WIDE_CHARS;
        true
    }

    /// Write a wide character without bounds checks or orphan fixup.
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

        // SAFETY: `col + 1 < self.cells.len()` is the caller's invariant (a).
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags.union(CellFlags::WIDE));
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }
    }

    /// Orphan fixup for a wide-char write at `col_usize`.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_write(
        &mut self,
        col_usize: usize,
        second_flags: CellFlags,
        cells_len: usize,
    ) -> bool {
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
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

    #[test]
    fn write_char_out_of_bounds_returns_false() {
        let (_pages, mut row) = make_row(10);
        assert!(!row.write_char(10, 'X'));
        assert!(!row.write_char(100, 'X'));
        assert_eq!(row.len(), 0);
    }

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

    #[test]
    fn write_char_packed_out_of_bounds() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(5);
        let colors = PackedColors::DEFAULT;
        assert!(!row.write_char_packed(5, 'X', colors, CellFlags::empty()));
        assert!(!row.write_char_packed(100, 'X', colors, CellFlags::empty()));
        assert_eq!(row.len(), 0);
    }

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

        row.write_wide_char(3, '\u{4E09}', fg, bg, CellFlags::empty());

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
        assert!(!row.write_wide_char_packed(4, '\u{4E2D}', colors, CellFlags::empty()));
        assert_eq!(row.len(), 0);
    }

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

    #[test]
    fn write_char_extends_len_for_sparse_writes() {
        let (_pages, mut row) = make_row(80);
        row.write_char(50, 'A');
        assert_eq!(row.len(), 51);
        row.write_char(10, 'B');
        assert_eq!(row.len(), 51);
    }
}
