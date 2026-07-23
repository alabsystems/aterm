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

        // Wide character fixup: only check when row has wide chars
        if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && let Some(current_flags) = self.cells.get(col_usize).map(|c| c.flags())
        {
            // Overwriting a genuine wide continuation orphans its head at col-1. Use
            // the context-aware check: WIDE_CONTINUATION (bit 10) aliases PROTECTED, so a
            // raw `contains(WIDE_CONTINUATION)` blanks col-1 when overwriting a DECSCA-
            // protected cell (round-8 bit-10 write-path fix). is_cell_wide_continuation
            // returns false for col==0, so (col-1) below cannot underflow.
            if self.is_cell_wide_continuation(col)
                && let Some(prev_cell) = self.cells.get_mut((col - 1) as usize)
            {
                *prev_cell = Cell::EMPTY;
            }
            if current_flags.contains(CellFlags::WIDE)
                && let Some(next_cell) = self.cells.get_mut(col_usize + 1)
            {
                *next_cell = Cell::EMPTY;
            }
        }

        if col_usize >= self.cells.len() {
            return false;
        }
        self.cells[col_usize].set_char(c);
        if col >= self.len {
            self.len = col.saturating_add(1);
        } else if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && old_len > 0
            && self.cells[old_len - 1].is_empty()
        {
            // Single-width write overwrote a WIDE head at the tail: the fixup
            // above cleared its continuation at col+1, and if that pair was the
            // row's last content (old_len == col+2) `col >= self.len` is false so
            // len would be left stale-high, surfacing a phantom trailing space in
            // row_text/search/scrollback (all slice through self.len). This is the
            // #7522 len class already closed on the clear/erase paths; the write
            // path was the remaining gap. recalculate_len_up_to rescans the prefix
            // and never over-shrinks, so gating only on "the old tail is now empty"
            // is correct and touches nothing on the ASCII (no-wide) hot path.
            self.recalculate_len_up_to(old_len);
        }
        self.flags |= RowFlags::DIRTY;
        true
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
        if self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            // SAFETY: col_usize < cells_len verified above
            let current_flags = unsafe { self.cells.get_unchecked(col_usize) }.flags();
            let wide_mask = CellFlags::WIDE.union(CellFlags::WIDE_CONTINUATION);
            if (current_flags.bits() & wide_mask.bits()) != 0 {
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
        if col >= self.len {
            self.len = col.saturating_add(1);
        } else if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && old_len > 0
            && self.cells[old_len - 1].is_empty()
        {
            // Interior single-width write over a WIDE head at the tail:
            // fixup_wide_char_overwrite cleared the continuation at col+1, which for
            // old_len == col+2 was the row's last content, so `col >= self.len` is
            // false and len would be left stale-high (phantom trailing space in
            // row_text/search/scrollback). #7522 len class, now closed on the write
            // path; recalculate_len_up_to never over-shrinks and the HAS_WIDE_CHARS
            // gate keeps the ASCII hot path branch-free.
            self.recalculate_len_up_to(old_len);
        }
        self.flags |= RowFlags::DIRTY;
        true
    }

    /// Handle wide character cleanup when overwriting part of a wide char.
    /// Marked cold since wide characters are relatively rare.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_overwrite(
        &mut self,
        col_usize: usize,
        current_flags: CellFlags,
        cells_len: usize,
    ) {
        // If overwriting a genuine wide continuation (second half), clear the orphaned
        // first half. Context-aware: WIDE_CONTINUATION (bit 10) aliases PROTECTED, so a
        // raw check blanks col-1 when overwriting a DECSCA-protected cell (round-8
        // write-path fix). The cell at col_usize is not overwritten until after this
        // fixup returns, so is_cell_wide_continuation reads the correct pre-write state.
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        // If overwriting a wide char (first half), clear the second half. WIDE (bit 9)
        // is unaliased, so the raw check is correct here.
        if current_flags.contains(CellFlags::WIDE) && col_usize + 1 < cells_len {
            self.cells[col_usize + 1] = Cell::EMPTY;
        }
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

        // Wide character fixup before overwriting the range.
        self.fixup_wide_chars_in_range(col, count);

        let end = start.saturating_add(usize::from(count)).min(cells_len);
        let actual = end - start;

        self.cells[start..end].fill(template);

        // Update len and flags.
        let end_col = u16_from_usize(end);
        if end_col > self.len && !template.is_empty() {
            self.len = end_col;
        } else if self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && old_len > 0
            && self.cells[old_len - 1].is_empty()
        {
            // fixup_wide_chars_in_range cleared the continuation at index `end`
            // (one column past the fill) when the last filled cell is a WIDE head.
            // If that orphan was the row's last content (old_len == end+1) the
            // grow above never fires and len is left stale-high — the #7522 len
            // class, previously closed only on the clear/erase paths. This is the
            // fast REP/padding path (write_cell_run), so the HAS_WIDE_CHARS gate
            // keeps ASCII runs branch-free; recalculate_len_up_to never over-shrinks.
            self.recalculate_len_up_to(old_len);
        }
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
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_chars_in_range(&mut self, start_col: u16, count: u16) {
        // Fast path: skip entire scan when row has no wide characters.
        // This is the common case for ASCII-only output (compiler, ls, etc.).
        if !self.flags.contains(RowFlags::HAS_WIDE_CHARS) {
            return;
        }
        let start = start_col as usize;
        let cells_len = self.cells.len();

        if start >= cells_len || count == 0 {
            return;
        }

        let end = usize::from(start_col.saturating_add(count));
        let actual_end = end.min(cells_len);

        // Check if the start cell is a genuine wide continuation - clear the orphaned
        // head. Context-aware: WIDE_CONTINUATION (bit 10) aliases PROTECTED, so a raw
        // check blanks start-1 when the fill range begins on a DECSCA-protected cell
        // (round-8 write-path fix). is_cell_wide_continuation returns false for start==0,
        // so start-1 cannot underflow. The cell at `start` is not filled until after this.
        if self.is_cell_wide_continuation(start_col) {
            self.cells[start - 1] = Cell::EMPTY;
        }

        // Check each cell being overwritten - if it's a WIDE char, clear its continuation
        for col in start..actual_end {
            let flags = self.cells[col].flags();
            if flags.contains(CellFlags::WIDE) && col + 1 < cells_len {
                // If the continuation is outside our write range, clear it
                // If it's inside, it will be overwritten anyway
                if col + 1 >= actual_end {
                    self.cells[col + 1] = Cell::EMPTY;
                }
            }
        }
    }

    /// Write a wide (double-width) character at the given column.
    ///
    /// Wide characters occupy two cells. The first cell contains the character
    /// with the WIDE flag set, and the second cell is a continuation cell.
    /// If overwriting parts of other wide characters, the orphaned halves are cleared.
    ///
    /// Returns `true` if the write succeeded, `false` if out of bounds.
    #[inline]
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
        if first_flags.contains(CellFlags::WIDE_CONTINUATION)
            || second_flags.contains(CellFlags::WIDE)
        {
            self.fixup_wide_char_write(col_usize, second_flags, cells_len);
        }

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
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, CellFlags::WIDE_CONTINUATION);
        }

        if col + 1 >= self.len {
            self.len = col.saturating_add(2);
        } else if old_len > 0 && self.cells[old_len - 1].is_empty() {
            // Interior wide write orphaned a pre-existing wide char's continuation
            // at col+2 (fixup_wide_char_write cleared it); if that was the row's
            // last content (old_len == col+3), `col+1 >= self.len` is false so len
            // would be left stale-high, surfacing a phantom trailing space in
            // row_text/search/scrollback. #7522 len class, now closed on the write
            // path. recalculate_len_up_to rescans and never over-shrinks; the
            // orphan can only exist when a prior wide char set HAS_WIDE_CHARS.
            self.recalculate_len_up_to(old_len);
        }
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_WIDE_CHARS;
        true
    }

    /// Write a wide character without fixup checks.
    ///
    /// Used by emoji/CJK batch run paths that already ensure sequential layout.
    /// This is the unchecked counterpart to [`Self::write_wide_char_packed`] —
    /// it skips bounds checks and wide-char orphan fixup in exchange for speed.
    ///
    /// # Safety
    ///
    /// The caller MUST uphold all of the following invariants:
    ///
    /// (a) `(col as usize) + 1 < self.cells_len()` — both `col` and `col + 1`
    ///     must be valid indices into the row's cell buffer. Violating this
    ///     triggers out-of-bounds UB via `get_unchecked_mut`.
    ///
    /// (b) `col` is on a wide-cell boundary — i.e. sequential writes land at
    ///     `col, col+2, col+4, ...` on a freshly sized run. Writing to an odd
    ///     offset inside an existing wide pair leaves a torn WIDE /
    ///     WIDE_CONTINUATION cell that downstream code does not expect.
    ///
    /// (c) The caller guarantees sequential non-overlapping writes so that no
    ///     WIDE / WIDE_CONTINUATION conflicts exist with cells already present
    ///     in the row. This function intentionally skips the orphan fixup that
    ///     [`Self::write_wide_char_packed`] performs; any pre-existing wide
    ///     pair overlap will produce a malformed row.
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
            *self.cells.get_unchecked_mut(col_usize + 1) =
                Cell::from_raw_parts(' ' as u16, colors, CellFlags::WIDE_CONTINUATION);
        }
    }

    /// Handle wide character cleanup when writing a wide char.
    /// Marked cold since these conflicts are relatively rare.
    #[cold]
    #[inline(never)]
    pub(crate) fn fixup_wide_char_write(
        &mut self,
        col_usize: usize,
        second_flags: CellFlags,
        cells_len: usize,
    ) {
        // If the first position overwrites a genuine wide continuation (second half),
        // clear the orphaned first half. Context-aware: WIDE_CONTINUATION (bit 10) aliases
        // PROTECTED, so a raw check blanks col-1 when the new wide char starts on a
        // DECSCA-protected cell (round-8 write-path fix). is_cell_wide_continuation returns
        // false for col==0. The cells are not written until after this fixup returns.
        if self.is_cell_wide_continuation(col_usize as u16) {
            self.cells[col_usize - 1] = Cell::EMPTY;
        }
        // If second position overwrites a wide char (first half), clear its continuation.
        // WIDE (bit 9) is unaliased, so the raw check is correct here.
        if second_flags.contains(CellFlags::WIDE) && col_usize + 2 < cells_len {
            self.cells[col_usize + 2] = Cell::EMPTY;
        }
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
    fn write_wide_char_basic() {
        let (_pages, mut row) = make_row(80);
        let (fg, bg) = default_colors();
        assert!(row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty()));
        assert!(row.get(0).unwrap().is_wide());
        assert!(row.get(1).unwrap().is_wide_continuation());
        assert_eq!(row.len(), 2);
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
    fn write_wide_char_at_last_column_rejected() {
        let (_pages, mut row) = make_row(10);
        let (fg, bg) = default_colors();
        // Col 9 is the last column; wide char needs col 9+10 but col 10 doesn't exist
        assert!(!row.write_wide_char(9, '\u{4E2D}', fg, bg, CellFlags::empty()));
        assert_eq!(row.len(), 0);
    }

    #[test]
    fn write_wide_char_at_second_to_last_succeeds() {
        let (_pages, mut row) = make_row(10);
        let (fg, bg) = default_colors();
        // Col 8 + col 9 = valid
        assert!(row.write_wide_char(8, '\u{4E2D}', fg, bg, CellFlags::empty()));
        assert!(row.get(8).unwrap().is_wide());
        assert!(row.get(9).unwrap().is_wide_continuation());
        assert_eq!(row.len(), 10);
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

    #[test]
    fn fill_cell_run_empty_template_does_not_extend_len() {
        let (_pages, mut row) = make_row(10);
        row.write_char(0, 'A');
        assert_eq!(row.len(), 1);

        // Fill with EMPTY cells beyond current len
        let written = row.fill_cell_run(5, 3, Cell::EMPTY);
        assert_eq!(written, 3);
        // len should stay at 1 since EMPTY cells don't extend
        assert_eq!(row.len(), 1);
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
