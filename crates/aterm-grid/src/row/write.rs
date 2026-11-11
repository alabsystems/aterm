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
        if col_usize >= self.cells.len() {
            return false;
        }
        let old_len = self.len as usize;
        let mut end_half_blanked = false;

        if self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            let current_flags = self.cells[col_usize].flags();
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

        if let Some(cell) = self.cells.get_mut(col_usize) {
            cell.set_char(c);
        }
        self.note_span(col, col + 1);
        self.note_write_through(col_usize + 1);
        self.settle_len_after_write(old_len, col_usize + 1, end_half_blanked);
        self.flags |= RowFlags::DIRTY;
        true
    }

    /// Write `text` (printable ASCII) in the default pen from `col` on a row that holds no wide
    /// glyph, ending at or before the row's end: what [`Self::cells_mut_with_fixup`] with a cell
    /// per byte and then [`Self::update_len`] leave, without the wide-glyph fixup such a row
    /// cannot need.
    #[inline]
    pub(crate) fn write_plain_ascii(&mut self, col: u16, text: &[u8]) {
        let start = usize::from(col);
        let end = start + text.len();
        debug_assert!(
            end <= self.cells.len() && !self.flags.contains(RowFlags::HAS_WIDE_CHARS),
            "write_plain_ascii: {start}..{end} does not fit a row of {} cells without wide glyphs",
            self.cells.len()
        );
        if end == start {
            return;
        }
        for (cell, &byte) in self.cells[start..end].iter_mut().zip(text) {
            *cell = Cell::from_ascii_fast(byte);
        }
        let end_u16 = u16_from_usize(end);
        self.note_span(col, end_u16);
        self.flags |= RowFlags::DIRTY;
        self.note_write_through(end);
        self.update_len(end_u16);
    }

    /// Updated `len` after writing `[.., end)`.
    #[inline]
    pub(super) fn settle_len_after_write(
        &mut self,
        old_len: usize,
        end: usize,
        end_half_blanked: bool,
    ) {
        #[cfg(test)]
        crate::test_counters::count_row_write();
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
            // SAFETY: col_usize < cells_len verified above.
            let current_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
            let wide_mask = CellFlags::WIDE.union(CellFlags::WIDE_CONTINUATION);
            if (current_flags.bits() & wide_mask.bits()) != 0 {
                end_half_blanked = self.fixup_wide_char_overwrite(
                    col_usize,
                    current_flags,
                    cells_len,
                    Cell::bce_blank(colors),
                );
            }
        }

        let cp = c as u32;
        let char_data = if cp <= Cell::MAX_DIRECT_CODEPOINT {
            cp as u16
        } else {
            '\u{FFFD}' as u16
        };

        // SAFETY: col_usize < cells_len verified above.
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags);
        }
        self.note_span(col, col + 1);
        self.note_write_through(col_usize + 1);
        self.settle_len_after_write(old_len, col_usize + 1, end_half_blanked);
        self.flags |= RowFlags::DIRTY;
        true
    }

    /// Orphan fixup for a single-cell write at `col_usize`: the other half of a wide glyph the
    /// write lands on becomes `orphan`, the writer's BCE blank (xterm `WriteText` clears the
    /// `DamagedCurCells` in the current colours, Ghostty's `printCell` clears them to its
    /// `blankCell`). True when the half after the write was blanked to an empty cell, so the
    /// row's length may end at the write.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_overwrite(
        &mut self,
        col_usize: usize,
        current_flags: CellFlags,
        cells_len: usize,
        orphan: Cell,
    ) -> bool {
        let col = u16_from_usize(col_usize);
        if self.is_cell_wide_continuation(col) {
            self.cells.set(col_usize - 1, orphan);
            self.note_span(col - 1, col);
        }
        if current_flags.contains(CellFlags::WIDE) && col_usize + 1 < cells_len {
            self.cells.set(col_usize + 1, orphan);
            self.note_span(col + 1, col + 2);
            return orphan.is_empty();
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
            && self.fixup_wide_chars_in_range(col, count, Cell::bce_blank(template.colors()));

        let end = start.saturating_add(usize::from(count)).min(cells_len);
        let actual = end - start;

        self.cells.slice_mut(start..end).fill(template);
        self.note_cells_written(start, end);
        self.note_write_through(end);

        self.settle_len_after_write(old_len, end, end_half_blanked);
        self.flags |= RowFlags::DIRTY;

        u16_from_usize(actual)
    }

    /// Set the wide-character halves a write over `start_col..start_col + count` orphans to
    /// `orphan` before the write: the writer's BCE blank for a print, `Cell::EMPTY` for a
    /// rectangle copy. True when the half after the span was blanked to an empty cell, so the
    /// row's length may end at the span.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_chars_in_range(
        &mut self,
        start_col: u16,
        count: u16,
        orphan: Cell,
    ) -> bool {
        if !self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            return false;
        }
        let halves = self.orphaned_wide_halves(start_col, start_col.saturating_add(count));
        for col in halves.into_iter().flatten() {
            self.cells.set(usize::from(col), orphan);
            self.note_span(col, col + 1);
        }
        halves[1].is_some() && orphan.is_empty()
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

        // SAFETY: col_usize < cells_len and col_usize + 1 < cells_len verified above.
        let first_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
        let second_flags = unsafe { self.cells.get_unchecked(col_usize + 1) }.flags();

        let end_half_blanked = (first_flags.contains(CellFlags::WIDE_CONTINUATION)
            || second_flags.contains(CellFlags::WIDE))
            && self.fixup_wide_char_write(
                col_usize,
                second_flags,
                cells_len,
                Cell::bce_blank(colors),
            );

        self.write_wide_run_packed_no_fixup(col, &[c], colors, |_| flags);
        self.note_span(col, col + 2);

        self.note_write_through(col_usize + 2);
        self.settle_len_after_write(old_len, col_usize + 2, end_half_blanked);
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_WIDE_CHARS;
        true
    }

    /// Write `chars` as wide glyphs from `col` on, two cells each, without orphan fixup, each
    /// with the flags `flags_of` gives it, noting the run's cells touched once (not a cell at a
    /// time). The run starts on a wide-cell boundary (sequential glyphs at `col, col + 2, ...` on
    /// a freshly sized span: an odd offset inside an existing pair leaves a torn pair), and
    /// nothing in the row conflicts with the skipped fixup. Panics when the run passes the row's
    /// end.
    ///
    /// The write leaves the row's written window alone: the run's
    /// [`Self::settle_len_after_wide_run`] widens it over the whole run once.
    #[inline]
    pub fn write_wide_run_packed_no_fixup(
        &mut self,
        col: u16,
        chars: &[char],
        colors: super::super::PackedColors,
        flags_of: impl Fn(char) -> CellFlags,
    ) {
        let start = usize::from(col);
        let run = self.cells.slice_mut(start..start + 2 * chars.len());
        for (pair, &c) in run.as_chunks_mut::<2>().0.iter_mut().zip(chars) {
            let cp = c as u32;
            let char_data = if cp <= Cell::MAX_DIRECT_CODEPOINT {
                cp as u16
            } else {
                '\u{FFFD}' as u16
            };
            let flags = flags_of(c);
            pair[0] = Cell::from_raw_parts(char_data, colors, flags.union(CellFlags::WIDE));
            pair[1] = Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }
    }

    /// Orphan fixup for a wide-char write at `col_usize`: the halves of wide glyphs the write
    /// cuts into become `orphan`, the writer's BCE blank, as for a narrow write
    /// ([`Self::fixup_wide_char_overwrite`]). True when the half after the write was blanked to
    /// an empty cell.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_write(
        &mut self,
        col_usize: usize,
        second_flags: CellFlags,
        cells_len: usize,
        orphan: Cell,
    ) -> bool {
        let col = u16_from_usize(col_usize);
        if self.is_cell_wide_continuation(col) {
            self.cells.set(col_usize - 1, orphan);
            self.note_span(col - 1, col);
        }
        if second_flags.contains(CellFlags::WIDE) && col_usize + 2 < cells_len {
            self.cells.set(col_usize + 2, orphan);
            self.note_span(col + 2, col + 3);
            return orphan.is_empty();
        }
        false
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
