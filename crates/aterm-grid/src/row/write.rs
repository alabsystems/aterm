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

        // Wide character fixup: only check when row has wide chars
        if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && let Some(current_flags) = self.cells.get(col_usize).map(|c| c.flags())
        {
            // Overwriting a genuine wide continuation orphans its head at col-1.
            // Context-aware check: bit 10 aliases PROTECTED, so a raw
            // `contains(WIDE_CONTINUATION)` would blank col-1 when overwriting a
            // protected cell. is_cell_wide_continuation is false for col==0, so
            // col-1 cannot underflow.
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

    /// `len` after a write over `[.., end)`: the one rule every writer keeps
    /// (ruling 2026-10-10, r15 C4).
    ///
    /// `len` is one past the last cell output has written: a printed glyph, a
    /// printed blank included, or a mark attached to a cell. That is what its
    /// readers take it for: the render frame stops at it, copy and search keep
    /// a soft-wrapped row's text through it (`TrailingBlanks::Written`), a
    /// history line stores that many cells, and a rewrap keeps the printed
    /// blanks it covers so a cursor after a printed space keeps its column. A
    /// printed blank therefore counts whichever writer printed it, one glyph at
    /// a time or as a run, and whatever the read boundaries were.
    ///
    /// The written span always counts. A wide half the write's fixup blanked
    /// was not written: when it was the row's last cell (`end` is its column
    /// and `end + 1 == old_len`), `len` ends at the write rather than leave a
    /// phantom trailing space. Erasing is not writing: the erase paths shrink
    /// `len` to the last cell that still holds content.
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

    /// Write a styled character at the given column with pre-computed colors.
    ///
    /// Avoids `convert_legacy_colors` per character — caller pre-computes once.
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

        // Single bounds check upfront
        if col_usize >= cells_len {
            return false;
        }

        // Wide character fixup (rare path): only check cell flags when the
        // row actually contains wide characters. Skips the flag read entirely
        // for ASCII-only rows (common case).
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

        // Build cell directly with pre-computed colors (skip convert_legacy_colors)
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

    /// Handle wide character cleanup when overwriting part of a wide char.
    /// Marked cold since wide characters are relatively rare. Returns whether
    /// it blanked the continuation after `col_usize` (the write's end half).
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_overwrite(
        &mut self,
        col_usize: usize,
        current_flags: CellFlags,
        cells_len: usize,
    ) -> bool {
        // Overwriting a genuine wide continuation: clear the orphaned head.
        // Context-aware check (bit 10 aliases PROTECTED); the cell at col_usize
        // is written only after this fixup, so the check reads the pre-write
        // state.
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        // If overwriting a wide char (first half), clear the second half. WIDE (bit 9)
        // is unaliased, so the raw check is correct here.
        if current_flags.contains(CellFlags::WIDE) && col_usize + 1 < cells_len {
            self.cells[col_usize + 1] = Cell::EMPTY;
            return true;
        }
        false
    }

    /// Fill `count` consecutive cells starting at `col` with a template cell.
    ///
    /// This is the bulk write path for runs of identical characters (e.g., REP,
    /// padding, separator lines). Uses `slice::fill()` which the compiler can
    /// lower to `memset`-like operations for 8-byte `Cell` values, avoiding
    /// per-cell branch overhead.
    ///
    /// Handles wide-character fixup at range boundaries via `HAS_WIDE_CHARS`.
    /// Returns the number of cells actually written (may be less than `count`
    /// if the range extends past the row end).
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

        // The filled cells are written, a default blank as much as a glyph
        // (the splat of a run of spaces, REP of a space), as every writer counts
        // them.
        self.settle_len_after_write(old_len, end, end_half_blanked);
        self.flags |= RowFlags::DIRTY;

        u16_from_usize(actual)
    }

    /// Fix up wide character orphans when overwriting a range of cells.
    ///
    /// This should be called before bulk-writing single-width characters to a range.
    /// It handles:
    /// - If start cell is a WIDE_CONTINUATION, clears the previous cell (orphaned first half)
    /// - If any cell in range has WIDE flag, clears the cell after it (orphaned continuation)
    ///
    /// This is the bulk equivalent of `fixup_wide_char_overwrite` for single cells.
    /// Returns whether it blanked the continuation at the span's end (the end
    /// half [`Self::settle_len_after_write`] takes).
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_chars_in_range(&mut self, start_col: u16, count: u16) -> bool {
        // Fast path: skip entire scan when row has no wide characters.
        // This is the common case for ASCII-only output (compiler, ls, etc.).
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

    /// The wide halves outside `[start, end)` that writing the span orphans:
    /// the head at `start - 1` when `start` is its continuation, and the
    /// continuation at `end` when `end - 1` is its head (`end` clipped to the
    /// row). These are the cells the write fixups blank:
    /// [`Self::fixup_wide_chars_in_range`], and, for a one- or two-cell span,
    /// [`Self::fixup_wide_char_overwrite`] and [`Self::fixup_wide_char_write`].
    /// The grid's writers probe them before a write so the extras entry of a
    /// blanked half goes with it (`grid::write::orphan_halves_with_extras`).
    ///
    /// The continuation test is context-aware (bit 10 aliases PROTECTED);
    /// the head test reads WIDE (bit 9), which is unaliased.
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
    /// Wide characters occupy two cells. The first cell contains the character
    /// with the WIDE flag set, and the second cell is a continuation cell.
    /// If overwriting parts of other wide characters, the orphaned halves are cleared.
    ///
    /// Returns `true` if the write succeeded, `false` if out of bounds.
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

        // Need at least 2 cells available - single bounds check
        if col_usize + 1 >= cells_len {
            return false;
        }

        // SAFETY: col_usize < cells_len and col_usize + 1 < cells_len verified above
        let first_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
        let second_flags = unsafe { self.cells.get_unchecked(col_usize + 1) }.flags();

        // Wide character fixup (rare path)
        let end_half_blanked = (first_flags.contains(CellFlags::WIDE_CONTINUATION)
            || second_flags.contains(CellFlags::WIDE))
            && self.fixup_wide_char_write(col_usize, second_flags, cells_len);

        // Build cells directly with pre-computed colors
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
            // The spacer carries the lead's rendition, not a bare role bit: a
            // double-width character is one character in two columns (see
            // `wide_continuation_of`). With WIDE_CONTINUATION alone, under SGR 7
            // the lead would resolve swapped and the spacer not, painting the
            // glyph's right half invisible.
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }

        self.note_write_through(col_usize + 2);
        self.settle_len_after_write(old_len, col_usize + 2, end_half_blanked);
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_WIDE_CHARS;
        true
    }

    /// Write a wide character without fixup checks.
    ///
    /// Used by emoji/CJK batch run paths that already ensure sequential layout.
    /// The unchecked counterpart to [`Self::write_wide_char_packed`]: it skips
    /// bounds checks and wide-char orphan fixup.
    ///
    /// # Safety
    ///
    /// The caller must uphold:
    ///
    /// (a) `(col as usize) + 1 < self.cells_len()`: both `col` and `col + 1`
    ///     are valid indices (otherwise `get_unchecked_mut` is UB).
    ///
    /// (b) `col` is on a wide-cell boundary: sequential writes land at
    ///     `col, col+2, col+4, ...` on a freshly sized run. An odd offset inside
    ///     an existing pair leaves a torn WIDE / WIDE_CONTINUATION cell.
    ///
    /// (c) Writes are sequential and non-overlapping, so no WIDE /
    ///     WIDE_CONTINUATION conflicts exist with cells already in the row; the
    ///     orphan fixup of [`Self::write_wide_char_packed`] is skipped.
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

        // SAFETY: caller guarantees `col + 1 < self.cells.len()` per the
        // function's `# Safety` contract (invariant (a)); the debug_assert
        // above catches violations in debug builds.
        unsafe {
            *self.cells.get_unchecked_mut(col_usize) =
                Cell::from_raw_parts(char_data, colors, flags.union(CellFlags::WIDE));
            // As in `write_wide_char_packed`, the spacer inherits the lead's
            // rendition.
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, flags.wide_continuation_of());
        }
    }

    /// Handle wide character cleanup when writing a wide char.
    /// Marked cold since these conflicts are relatively rare. Returns whether
    /// it blanked the continuation after the written pair (its end half).
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_write(
        &mut self,
        col_usize: usize,
        second_flags: CellFlags,
        cells_len: usize,
    ) -> bool {
        // If the first position overwrites a genuine wide continuation, clear
        // the orphaned head. Context-aware check (bit 10 aliases PROTECTED);
        // false for col==0. Cells are written only after this fixup.
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        // If second position overwrites a wide char (first half), clear its continuation.
        // WIDE (bit 9) is unaliased, so the raw check is correct here.
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

    // ── write_char: ASCII ────────────────────────────────────────────

    #[test]
    fn write_char_ascii_single() {
        let (_pages, mut row) = make_row(80);
        assert!(row.write_char(0, 'A'));
        assert_eq!(row.get(0).unwrap().char(), 'A');
        assert_eq!(row.len(), 1);
    }

    #[test]
    fn write_char_ascii_multiple_sequential() {
        let (_pages, mut row) = make_row(80);
        for (i, c) in "Hello".chars().enumerate() {
            assert!(row.write_char(i as u16, c));
        }
        assert_eq!(row.len(), 5);
        assert_eq!(row.to_string(), "Hello");
    }

    #[test]
    fn write_char_out_of_bounds_returns_false() {
        let (_pages, mut row) = make_row(10);
        assert!(!row.write_char(10, 'X'));
        assert!(!row.write_char(100, 'X'));
        assert_eq!(row.len(), 0);
    }

    #[test]
    fn write_char_at_last_column() {
        let (_pages, mut row) = make_row(10);
        assert!(row.write_char(9, 'Z'));
        assert_eq!(row.get(9).unwrap().char(), 'Z');
        assert_eq!(row.len(), 10);
    }

    #[test]
    fn write_char_sets_dirty_flag() {
        let (_pages, mut row) = make_row(10);
        row.write_char(0, 'A');
        assert!(row.flags().contains(RowFlags::DIRTY));
    }

    // ── write_char: multibyte UTF-8 (BMP) ───────────────────────────

    #[test]
    fn write_char_bmp_accented() {
        let (_pages, mut row) = make_row(80);
        assert!(row.write_char(0, '\u{00E9}')); // e with acute
        assert_eq!(row.get(0).unwrap().char(), '\u{00E9}');
        assert_eq!(row.len(), 1);
    }

    #[test]
    fn write_char_bmp_greek() {
        let (_pages, mut row) = make_row(80);
        assert!(row.write_char(0, '\u{03B1}')); // alpha
        assert!(row.write_char(1, '\u{03B2}')); // beta
        assert_eq!(row.get(0).unwrap().char(), '\u{03B1}');
        assert_eq!(row.get(1).unwrap().char(), '\u{03B2}');
    }

    #[test]
    fn write_char_bmp_cjk_ideograph_single_cell() {
        // CJK ideograph fits in BMP but write_char treats it as single-width.
        // Wide rendering requires write_wide_char.
        let (_pages, mut row) = make_row(80);
        assert!(row.write_char(0, '\u{4E2D}')); // 中
        assert_eq!(row.get(0).unwrap().char(), '\u{4E2D}');
        assert_eq!(row.len(), 1);
    }

    // ── write_char: overwrite existing content ──────────────────────

    #[test]
    fn write_char_overwrites_existing() {
        let (_pages, mut row) = make_row(80);
        row.write_char(0, 'A');
        row.write_char(0, 'B');
        assert_eq!(row.get(0).unwrap().char(), 'B');
        assert_eq!(row.len(), 1);
    }

    #[test]
    fn write_char_overwrite_wide_first_half_clears_continuation() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();

        // Place wide char at col 0-1
        row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert!(row.get(0).unwrap().is_wide());
        assert!(row.get(1).unwrap().is_wide_continuation());

        // Overwrite the first half (col 0) with a narrow char
        row.write_char(0, 'X');
        assert_eq!(row.get(0).unwrap().char(), 'X');
        // Continuation at col 1 should be cleared
        assert_eq!(row.get(1).unwrap().char(), ' ');
        assert!(!row.get(1).unwrap().is_wide_continuation());
    }

    #[test]
    fn write_char_overwrite_wide_continuation_clears_main() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();

        row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert!(row.get(2).unwrap().is_wide());
        assert!(row.get(3).unwrap().is_wide_continuation());

        // Overwrite the continuation (col 3) with a narrow char
        row.write_char(3, 'Y');
        assert_eq!(row.get(3).unwrap().char(), 'Y');
        // The main wide cell at col 2 should be cleared
        assert_eq!(row.get(2).unwrap().char(), ' ');
        assert!(!row.get(2).unwrap().is_wide());
    }

    // ── write_char_styled ───────────────────────────────────────────

    #[test]
    fn write_char_styled_basic() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();
        assert!(row.write_char_styled(0, 'S', fg, bg, CellFlags::BOLD));
        assert_eq!(row.get(0).unwrap().char(), 'S');
        assert!(row.get(0).unwrap().flags().contains(CellFlags::BOLD));
    }

    // ── write_char_packed ───────────────────────────────────────────

    #[test]
    fn write_char_packed_ascii() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(80);
        let colors = PackedColors::DEFAULT;
        assert!(row.write_char_packed(0, 'P', colors, CellFlags::empty()));
        assert_eq!(row.get(0).unwrap().char(), 'P');
        assert_eq!(row.len(), 1);
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
    fn write_char_packed_overwrite_wide_continuation_clears_main() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();
        let colors = PackedColors::DEFAULT;

        row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
        // Overwrite the continuation at col 5
        assert!(row.write_char_packed(5, 'Z', colors, CellFlags::empty()));
        assert_eq!(row.get(5).unwrap().char(), 'Z');
        assert_eq!(row.get(4).unwrap().char(), ' ');
    }

    #[test]
    fn write_char_packed_overwrite_wide_main_clears_continuation() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();
        let colors = PackedColors::DEFAULT;

        row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
        // Overwrite the main wide cell at col 4
        assert!(row.write_char_packed(4, 'Q', colors, CellFlags::empty()));
        assert_eq!(row.get(4).unwrap().char(), 'Q');
        assert_eq!(row.get(5).unwrap().char(), ' ');
    }

    #[test]
    fn write_char_packed_updates_len() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(80);
        let colors = PackedColors::DEFAULT;

        row.write_char_packed(5, 'A', colors, CellFlags::empty());
        assert_eq!(row.len(), 6);
        // Writing at a lower column should not change len
        row.write_char_packed(2, 'B', colors, CellFlags::empty());
        assert_eq!(row.len(), 6);
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

        // Place wide char at col 2-3
        row.write_wide_char(2, '\u{4E00}', fg, bg, CellFlags::empty());
        // Place wide char at col 4-5
        row.write_wide_char(4, '\u{4E8C}', fg, bg, CellFlags::empty());

        // Now write a wide char at col 3-4, which overlaps:
        // - col 3 is the continuation of the first wide char
        // - col 4 is the main cell of the second wide char
        row.write_wide_char(3, '\u{4E09}', fg, bg, CellFlags::empty());

        // Col 2 should be cleared (orphaned first half of original wide)
        assert_eq!(row.get(2).unwrap().char(), ' ');
        // Col 3 should be the new wide char
        assert!(row.get(3).unwrap().is_wide());
        // Col 4 should be continuation of the new wide char
        assert!(row.get(4).unwrap().is_wide_continuation());
        // Col 5 should be cleared (orphaned continuation of second wide)
        assert_eq!(row.get(5).unwrap().char(), ' ');
    }

    // ── write_wide_char_packed ──────────────────────────────────────

    #[test]
    fn write_wide_char_packed_basic() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(80);
        let colors = PackedColors::DEFAULT;
        assert!(row.write_wide_char_packed(0, '\u{4E2D}', colors, CellFlags::empty()));
        assert!(row.get(0).unwrap().is_wide());
        assert!(row.get(1).unwrap().is_wide_continuation());
        assert_eq!(row.len(), 2);
    }

    #[test]
    fn write_wide_char_packed_out_of_bounds() {
        use super::super::super::PackedColors;
        let (_pages, mut row) = make_row(5);
        let colors = PackedColors::DEFAULT;
        // Col 4 is last; wide needs col 4+5 but 5 doesn't exist
        assert!(!row.write_wide_char_packed(4, '\u{4E2D}', colors, CellFlags::empty()));
        assert_eq!(row.len(), 0);
    }

    // ── fill_cell_run ───────────────────────────────────────────────

    #[test]
    fn fill_cell_run_basic_fill() {
        let (_pages, mut row) = make_row(20);
        let template = Cell::from_ascii_fast(b'X');
        let written = row.fill_cell_run(0, 10, template);
        assert_eq!(written, 10);
        assert_eq!(row.len(), 10);
        for col in 0..10 {
            assert_eq!(row.get(col).unwrap().char(), 'X');
        }
    }

    #[test]
    fn fill_cell_run_overwrites_wide_at_start_boundary() {
        let (_pages, mut row) = make_row(20);
        let (fg, bg) = default_colors();

        // Place wide char at col 2-3
        row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());

        // Fill starting at the continuation (col 3)
        let template = Cell::from_ascii_fast(b'.');
        row.fill_cell_run(3, 5, template);

        // The orphaned wide main at col 2 should be cleared
        assert_eq!(row.get(2).unwrap().char(), ' ');
        for col in 3..8 {
            assert_eq!(row.get(col).unwrap().char(), '.');
        }
    }

    #[test]
    fn fill_cell_run_overwrites_wide_at_end_boundary() {
        let (_pages, mut row) = make_row(20);
        let (fg, bg) = default_colors();

        // Place wide char at col 5-6
        row.write_wide_char(5, '\u{4E2D}', fg, bg, CellFlags::empty());

        // Fill col 3..6 (covers the wide main at 5 but not continuation at 6)
        let template = Cell::from_ascii_fast(b'.');
        row.fill_cell_run(3, 3, template);

        // Col 6 should be cleared (orphaned continuation)
        assert_eq!(row.get(6).unwrap().char(), ' ');
    }

    /// A run of printed default blanks is written text (ruling 2026-10-10,
    /// r15 C4): `len` covers it as it covers the same blanks written one at a
    /// time, so a splatted or REP'd run of spaces is part of the line.
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

    // ── fixup_wide_char_overwrite ───────────────────────────────────

    #[test]
    fn fixup_wide_char_overwrite_clears_continuation() {
        let (_pages, mut row) = make_row(10);
        let (fg, bg) = default_colors();
        row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());

        // Manually call fixup as if we're overwriting col 0 (WIDE cell)
        let current_flags = row.get(0).unwrap().flags();
        row.fixup_wide_char_overwrite(0, current_flags, row.cols() as usize);

        // Continuation at col 1 should be cleared
        assert_eq!(row.get(1).unwrap().char(), ' ');
        assert!(!row.get(1).unwrap().is_wide_continuation());
    }

    #[test]
    fn fixup_wide_char_overwrite_clears_main_when_at_continuation() {
        let (_pages, mut row) = make_row(10);
        let (fg, bg) = default_colors();
        row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());

        // Fixup at col 3 (WIDE_CONTINUATION)
        let current_flags = row.get(3).unwrap().flags();
        row.fixup_wide_char_overwrite(3, current_flags, row.cols() as usize);

        // Main at col 2 should be cleared
        assert_eq!(row.get(2).unwrap().char(), ' ');
        assert!(!row.get(2).unwrap().is_wide());
    }

    // ── fixup_wide_char_write ───────────────────────────────────────

    #[test]
    fn fixup_wide_char_write_handles_overlapping_wides() {
        let (_pages, mut row) = make_row(10);
        let (fg, bg) = default_colors();

        // Place wide at col 0-1 and col 2-3
        row.write_wide_char(0, '\u{4E00}', fg, bg, CellFlags::empty());
        row.write_wide_char(2, '\u{4E8C}', fg, bg, CellFlags::empty());

        // Now simulate writing a wide at col 1-2: col 1 is WIDE_CONTINUATION,
        // col 2 is WIDE
        let second_flags = row.get(2).unwrap().flags();
        row.fixup_wide_char_write(1, second_flags, row.cols() as usize);

        // Col 0 should be cleared (orphaned first half)
        assert_eq!(row.get(0).unwrap().char(), ' ');
        // Col 3 should be cleared (orphaned continuation)
        assert_eq!(row.get(3).unwrap().char(), ' ');
    }

    // ── len tracking edge cases ─────────────────────────────────────

    #[test]
    fn write_char_extends_len_for_sparse_writes() {
        let (_pages, mut row) = make_row(80);
        row.write_char(50, 'A');
        assert_eq!(row.len(), 51);
        row.write_char(10, 'B');
        // len should stay at 51 since 10 < 50
        assert_eq!(row.len(), 51);
    }

    #[test]
    fn write_wide_char_len_accounts_for_both_cells() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();
        row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert_eq!(row.len(), 2);
        row.write_wide_char(10, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert_eq!(row.len(), 12);
    }
}
