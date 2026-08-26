// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Derived-content queries on [`Grid`]: text extraction and memory accounting.
//!
//! These methods compute data from grid state (row text, visible content,
//! memory usage) rather than forwarding to subsystem accessors. Separated
//! from `accessors.rs` for cohesion — accessors are O(1) field forwarding,
//! these iterate over grid contents.

use super::Grid;
use crate::CellCoord;

impl Grid {
    /// Get the resolved character at a grid position, checking overflow.
    ///
    /// Unlike `Cell::char()`, this returns the full character for non-BMP
    /// codepoints by consulting the `CellExtras` overflow table.
    #[must_use]
    pub fn resolved_char(&self, row: u16, col: u16) -> Option<char> {
        let cell = self.cell(row, col)?;
        if cell.is_complex() {
            self.complex_char_at(row, col).or(Some('\u{FFFD}'))
        } else {
            Some(cell.char())
        }
    }

    /// Estimate total memory used by the grid and attached scrollback.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let mut total = self.storage.pages.total_memory();
        total += self.storage.extras.memory_used();
        total += self.storage.rows.capacity() * std::mem::size_of::<crate::row::Row>();
        total += self.storage.tab_stops.capacity() * std::mem::size_of::<bool>();
        total += self.storage.ring_extras.capacity()
            * std::mem::size_of::<Option<Box<super::scroll_convert::ScrolledRowExtras>>>();
        for entry in &self.storage.ring_extras {
            if entry.is_some() {
                total += std::mem::size_of::<super::scroll_convert::ScrolledRowExtras>();
            }
        }
        // Staged-but-undrained scroll-off lines (Wave-3 adversarial review):
        // under flood backpressure the lazy buffer holds raw ~8 B/cell rows the
        // tiered store has not absorbed yet — bytes the ring watermark must see.
        total += self.storage.lazy_buffer.memory_used();
        if let Some(scrollback) = &self.storage.scrollback {
            total += scrollback.total_memory_used();
        }
        total
    }

    /// Test-only: ring-buffer row Vec capacity (entries), for startup-footprint
    /// measurement in `mem_measure_tests`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn ring_rows_capacity_for_test(&self) -> usize {
        self.storage.rows.capacity()
    }

    /// Test-only: page-store total memory (bytes), for startup-footprint
    /// measurement in `mem_measure_tests`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn pages_total_memory_for_test(&self) -> usize {
        self.storage.pages.total_memory()
    }

    /// Get the text content of a visible row, resolving complex characters.
    ///
    /// Handles non-BMP characters stored in the overflow table. Thin wrapper
    /// over [`row_text_into`](Self::row_text_into) for callers that want an
    /// owned `String`; hot paths that scan many rows per frame should prefer
    /// `row_text_into` with a reused buffer to avoid one allocation per row.
    #[must_use]
    pub fn row_text(&self, row: u16) -> Option<String> {
        let mut s = String::new();
        self.row_text_into(row, &mut s).then_some(s)
    }

    /// Append visible-row `row`'s resolved text into `out`, reusing its heap
    /// capacity (clear-then-append). Writes exactly what [`row_text`](Self::row_text)
    /// would return for an in-bounds row and returns `true`; for an out-of-range
    /// row it leaves `out` cleared and returns `false` (mirroring `row_text`'s
    /// `None`). The non-allocating counterpart used by the Observation Kernel's
    /// per-batch row scan so a `RowMatches` watcher does not heap-allocate one
    /// `String` per visible row on every processed batch.
    #[must_use]
    pub fn row_text_into(&self, row: u16, out: &mut String) -> bool {
        out.clear();
        // Out-of-viewport rows have no text. In-range rows resolve through the
        // display-offset-aware view: a scrolled-back row reads its extras from
        // the materialized history line (correctly-paired complex chars /
        // combining marks / RGB), not the live map where they no longer live —
        // so `self.row(row).is_none()` is NOT a valid emptiness test here (a
        // deep-scrolled history row legitimately has no live grid row).
        if row >= self.rows() {
            return false;
        }
        let view = self.visible_row_view(row);
        let len = view.len();
        out.reserve(len as usize);
        for col in 0..len {
            let Some(cell) = view.cell(col) else {
                continue;
            };
            // Context-aware check: Cell::is_wide_continuation() would
            // false-positive on DECSCA-protected cells (shared bit 10),
            // hiding protected text from the read API.
            if view.is_wide_continuation(col) {
                continue;
            }
            view.push_cell_text(col, cell, out);
        }
        true
    }

    /// Fill `out` with visible-row `row`'s content as PER-COLUMN chars and
    /// return the row's FILL (one past the last non-blank column; `0` for an
    /// empty or out-of-range row).
    ///
    /// The column-indexed sibling of [`row_text_into`](Self::row_text_into),
    /// built for per-frame row DIFFING (the cursor-effects erase-poof probe):
    /// string output loses column indexing under wide chars, so this yields
    /// exactly one `char` per grid column — the resolved LEAD char at its own
    /// column (a complex cell contributes its base codepoint), `'\0'` at wide
    /// continuations (the right half of a CJK/emoji glyph), and `' '` for
    /// blanks — so span math on the diff survives CJK/emoji. Combining marks
    /// are deliberately dropped (they share the base's column; a diff keyed on
    /// lead chars still sees any real content change that erases them).
    /// Clear-then-extend into the caller's buffer: zero steady-state
    /// allocation, like `row_text_into`. Reads through the display-offset-aware
    /// `visible_row_view`, exactly like the text path.
    pub fn row_cols_into(&self, row: u16, out: &mut Vec<char>) -> u16 {
        self.row_cols_prefix_into(row, usize::MAX, out)
    }

    /// Fill `out` with at most the first `prefix_len` per-column characters of
    /// visible row `row` and return the fill within that prefix.
    ///
    /// This is the bounded hot-path counterpart of
    /// [`row_cols_into`](Self::row_cols_into): it uses the identical lead /
    /// wide-continuation / blank projection, but neither scans nor reserves for
    /// columns beyond the requested prefix. It does not synthesize the sparse
    /// row's implicit blank tail; callers that require a fixed-width prefix may
    /// resize the returned buffer to `prefix_len`.
    pub fn row_cols_prefix_into(&self, row: u16, prefix_len: usize, out: &mut Vec<char>) -> u16 {
        out.clear();
        if row >= self.rows() {
            return 0;
        }
        let view = self.visible_row_view(row);
        let prefix_len = u16::try_from(prefix_len).unwrap_or(u16::MAX);
        let len = view.len().min(prefix_len);
        out.reserve(len as usize);
        let mut fill = 0u16;
        for col in 0..len {
            let ch = match view.cell(col) {
                None => ' ',
                Some(_) if view.is_wide_continuation(col) => '\0',
                Some(cell) if cell.is_complex() => view
                    .cell_data(col, cell)
                    .complex_base()
                    .unwrap_or('\u{FFFD}'),
                Some(cell) => {
                    let c = cell.char();
                    if c == '\0' { ' ' } else { c }
                }
            };
            out.push(ch);
            if ch != ' ' {
                // A wide continuation ('\0') counts as filled: its lead glyph
                // is real content occupying this column.
                fill = col + 1;
            }
        }
        fill
    }

    /// Get a LIVE on-screen row (keyed on the SCREEN row, as if `display_offset ==
    /// 0`) as text with ANSI SGR escape codes — offset-INDEPENDENT, the coloured
    /// twin of [`row_text_screen`](Self::row_text_screen). `serialize()` captures
    /// terminal STATE, so its viewport paint must read the live screen regardless of
    /// the user's scroll position: reading the offset-mapped [`Grid::row`] would
    /// re-emit scrolled history (duplicating the history dump) AND pair those
    /// historical cells with the LIVE extras map at the raw row — stranding a
    /// scrolled emoji as U+FFFD and dropping its RGB/combining. `row` here is a
    /// SCREEN row; byte-identical to the pre-fix `row_ansi_text` at `display_offset
    /// == 0` (where `row_at_screen == row`).
    ///
    /// Iterates cells and emits `\x1b[...m` sequences when SGR state
    /// changes (bold, italic, colors, etc.). Appends `\x1b[0m` at the
    /// end if any SGR was active. Returns `None` for invalid row indices.
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "flat SGR tracking per cell")]
    pub fn row_ansi_text_screen(&self, row: u16) -> Option<String> {
        use crate::CellFlags;

        let r = self.storage.row_at_screen(row)?;
        let mut s = String::with_capacity(r.len() as usize * 2);
        let extras_empty = self.storage.extras.is_empty();

        // Current SGR state tracking.
        let mut cur_bold = false;
        let mut cur_dim = false;
        let mut cur_italic = false;
        let mut cur_underline = false;
        let mut cur_blink = false;
        let mut cur_inverse = false;
        let mut cur_hidden = false;
        let mut cur_strikethrough = false;
        let mut cur_overline = false;
        let mut cur_fg_indexed: Option<u8> = None;
        let mut cur_bg_indexed: Option<u8> = None;
        let mut cur_fg_rgb: Option<[u8; 3]> = None;
        let mut cur_bg_rgb: Option<[u8; 3]> = None;
        let mut any_sgr = false;

        for col in 0..r.len() {
            let cell = match r.get(col) {
                Some(c) => c,
                None => continue,
            };

            // Context-aware check — see row_text above (shared bit 10).
            if r.is_cell_wide_continuation(col) {
                continue;
            }

            let flags = cell.flags();
            let colors = cell.colors();

            // Compute desired SGR state from this cell.
            let want_bold = flags.contains(CellFlags::BOLD);
            let want_dim = flags.contains(CellFlags::DIM);
            let want_italic = flags.contains(CellFlags::ITALIC);
            let want_underline = flags.contains(CellFlags::UNDERLINE);
            let want_blink = flags.contains(CellFlags::BLINK);
            let want_inverse = flags.contains(CellFlags::INVERSE);
            let want_hidden = flags.contains(CellFlags::HIDDEN);
            let want_strikethrough = flags.contains(CellFlags::STRIKETHROUGH);
            let want_overline = flags.contains(CellFlags::OVERLINE);

            let want_fg_indexed = if colors.fg_is_indexed() {
                Some(colors.fg_index())
            } else {
                None
            };
            let want_bg_indexed = if colors.bg_is_indexed() {
                Some(colors.bg_index())
            } else {
                None
            };
            let want_fg_rgb = if colors.fg_is_rgb() {
                self.fg_rgb_at(row, col)
            } else {
                None
            };
            let want_bg_rgb = if colors.bg_is_rgb() {
                self.bg_rgb_at(row, col)
            } else {
                None
            };

            // Check if anything changed.
            let changed = want_bold != cur_bold
                || want_dim != cur_dim
                || want_italic != cur_italic
                || want_underline != cur_underline
                || want_blink != cur_blink
                || want_inverse != cur_inverse
                || want_hidden != cur_hidden
                || want_strikethrough != cur_strikethrough
                || want_overline != cur_overline
                || want_fg_indexed != cur_fg_indexed
                || want_bg_indexed != cur_bg_indexed
                || want_fg_rgb != cur_fg_rgb
                || want_bg_rgb != cur_bg_rgb;

            if changed {
                // Emit a reset + new state to keep things simple.
                // For cells with no attributes, emit reset only if we had attributes.
                let has_attrs = want_bold
                    || want_dim
                    || want_italic
                    || want_underline
                    || want_blink
                    || want_inverse
                    || want_hidden
                    || want_strikethrough
                    || want_overline
                    || want_fg_indexed.is_some()
                    || want_bg_indexed.is_some()
                    || want_fg_rgb.is_some()
                    || want_bg_rgb.is_some();

                if has_attrs {
                    // Build SGR params.
                    let mut params: Vec<String> = vec!["0".to_string()];
                    if want_bold {
                        params.push("1".to_string());
                    }
                    if want_dim {
                        params.push("2".to_string());
                    }
                    if want_italic {
                        params.push("3".to_string());
                    }
                    if want_underline {
                        params.push("4".to_string());
                    }
                    if want_blink {
                        params.push("5".to_string());
                    }
                    if want_inverse {
                        params.push("7".to_string());
                    }
                    if want_hidden {
                        params.push("8".to_string());
                    }
                    if want_strikethrough {
                        params.push("9".to_string());
                    }
                    if want_overline {
                        params.push("53".to_string());
                    }
                    // Foreground color.
                    if let Some(rgb) = want_fg_rgb {
                        params.push(format!("38;2;{};{};{}", rgb[0], rgb[1], rgb[2]));
                    } else if let Some(idx) = want_fg_indexed {
                        if idx < 8 {
                            params.push(format!("{}", 30 + idx));
                        } else if idx < 16 {
                            params.push(format!("{}", 90 + idx - 8));
                        } else {
                            params.push(format!("38;5;{idx}"));
                        }
                    }
                    // Background color.
                    if let Some(rgb) = want_bg_rgb {
                        params.push(format!("48;2;{};{};{}", rgb[0], rgb[1], rgb[2]));
                    } else if let Some(idx) = want_bg_indexed {
                        if idx < 8 {
                            params.push(format!("{}", 40 + idx));
                        } else if idx < 16 {
                            params.push(format!("{}", 100 + idx - 8));
                        } else {
                            params.push(format!("48;5;{idx}"));
                        }
                    }
                    s.push_str(&format!("\x1b[{}m", params.join(";")));
                    any_sgr = true;
                } else if any_sgr {
                    s.push_str("\x1b[0m");
                    any_sgr = false;
                }

                cur_bold = want_bold;
                cur_dim = want_dim;
                cur_italic = want_italic;
                cur_underline = want_underline;
                cur_blink = want_blink;
                cur_inverse = want_inverse;
                cur_hidden = want_hidden;
                cur_strikethrough = want_strikethrough;
                cur_overline = want_overline;
                cur_fg_indexed = want_fg_indexed;
                cur_bg_indexed = want_bg_indexed;
                cur_fg_rgb = want_fg_rgb;
                cur_bg_rgb = want_bg_rgb;
            }

            // Emit the character.
            if cell.is_complex() {
                if let Some(cs) = self.complex_char_str_at(row, col) {
                    s.push_str(&cs);
                } else {
                    s.push('\u{FFFD}');
                }
                if cell.has_extras()
                    && !extras_empty
                    && let Some(extra) = self.storage.extras.get(CellCoord::new(row, col))
                {
                    for &combining in extra.combining() {
                        s.push(combining);
                    }
                }
            } else {
                let ch = cell.char();
                s.push(if ch == '\0' { ' ' } else { ch });
                if cell.has_extras()
                    && !extras_empty
                    && let Some(extra) = self.storage.extras.get(CellCoord::new(row, col))
                {
                    for &combining in extra.combining() {
                        s.push(combining);
                    }
                }
            }
        }

        if any_sgr {
            s.push_str("\x1b[0m");
        }

        Some(s)
    }

    /// Get visible row content as a string (for debugging).
    #[must_use]
    pub fn visible_content(&self) -> String {
        let mut s = String::new();
        for row in 0..self.storage.visible_rows {
            if let Some(text) = self.row_text(row) {
                s.push_str(&text);
            }
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::Grid;
    use crate::CellCoord;
    use crate::cell::CellFlags;
    use crate::cell_colors::PackedColor;

    /// Helper: write ASCII text at the current cursor position.
    fn write_text(grid: &mut Grid, text: &str) {
        for c in text.chars() {
            grid.write_char(c);
        }
    }

    // =========================================================================
    // resolved_char — basic character resolution
    // =========================================================================

    #[test]
    fn test_resolved_char_ascii() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('A');
        assert_eq!(grid.resolved_char(0, 0), Some('A'));
    }

    #[test]
    fn test_resolved_char_empty_cell() {
        let grid = Grid::new(4, 20);
        // Cell::EMPTY stores space (0x20), so unwritten cells return space
        let ch = grid.resolved_char(0, 0);
        assert_eq!(ch, Some(' '));
    }

    #[test]
    fn test_resolved_char_out_of_bounds_row() {
        let grid = Grid::new(4, 20);
        assert_eq!(grid.resolved_char(10, 0), None);
    }

    #[test]
    fn test_resolved_char_out_of_bounds_col() {
        let grid = Grid::new(4, 20);
        assert_eq!(grid.resolved_char(0, 30), None);
    }

    #[test]
    fn test_resolved_char_supplementary_plane() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('\u{1F600}'); // grinning face (non-BMP)
        assert_eq!(grid.resolved_char(0, 0), Some('\u{1F600}'));
    }

    #[test]
    fn test_resolved_char_bmp_cjk() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('\u{4E2D}'); // CJK "middle"
        assert_eq!(grid.resolved_char(0, 0), Some('\u{4E2D}'));
    }

    #[test]
    fn test_resolved_char_multiple_positions() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('X');
        grid.write_char('Y');
        grid.write_char('Z');
        assert_eq!(grid.resolved_char(0, 0), Some('X'));
        assert_eq!(grid.resolved_char(0, 1), Some('Y'));
        assert_eq!(grid.resolved_char(0, 2), Some('Z'));
    }

    // =========================================================================
    // row_text — correct text extraction
    // =========================================================================

    #[test]
    fn test_row_text_ascii() {
        let mut grid = Grid::new(4, 20);
        write_text(&mut grid, "Hello");
        let text = grid.row_text(0).unwrap();
        assert!(text.starts_with("Hello"), "got: {text:?}");
    }

    #[test]
    fn test_row_text_empty_row() {
        let grid = Grid::new(4, 20);
        let text = grid.row_text(0).unwrap();
        // Row::len() tracks actual written content; an untouched row has
        // len=0, so row_text iterates zero cells and returns empty string.
        assert_eq!(text, "", "untouched row should produce empty string");
    }

    #[test]
    fn test_row_text_out_of_bounds() {
        let grid = Grid::new(4, 20);
        assert!(grid.row_text(10).is_none());
    }

    #[test]
    fn test_row_text_multiple_rows() {
        let mut grid = Grid::new(4, 20);
        write_text(&mut grid, "Line1");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "Line2");
        grid.move_cursor_to(2, 0);
        write_text(&mut grid, "Line3");

        assert!(grid.row_text(0).unwrap().starts_with("Line1"));
        assert!(grid.row_text(1).unwrap().starts_with("Line2"));
        assert!(grid.row_text(2).unwrap().starts_with("Line3"));
    }

    #[test]
    fn test_row_text_supplementary_plane_emoji() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('\u{1F680}'); // rocket emoji
        let text = grid.row_text(0).unwrap();
        assert!(
            text.contains('\u{1F680}'),
            "row_text should contain rocket emoji, got: {text:?}"
        );
    }

    #[test]
    fn test_row_text_null_cells_become_spaces() {
        let mut grid = Grid::new(4, 10);
        // Write at position 5, leave 0-4 as nulls
        grid.move_cursor_to(0, 5);
        grid.write_char('X');
        let text = grid.row_text(0).unwrap();
        assert!(
            text.starts_with("     X"),
            "nulls should become spaces, got: {text:?}"
        );
    }

    #[test]
    fn test_row_text_full_row() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "ABCDE");
        let text = grid.row_text(0).unwrap();
        assert_eq!(text, "ABCDE");
    }

    #[test]
    fn test_row_text_into_matches_row_text_and_reuses_capacity() {
        let mut grid = Grid::new(4, 20);
        write_text(&mut grid, "Hello");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "World!");

        // Parity with the owning wrapper for every visible row.
        let mut buf = String::new();
        for row in 0..grid.rows() {
            let owned = grid.row_text(row);
            let ok = grid.row_text_into(row, &mut buf);
            assert!(ok, "in-bounds row must succeed");
            assert_eq!(Some(buf.as_str()), owned.as_deref(), "row {row} parity");
        }

        // Reuse: refilling a buffer that already holds a longer string clears it
        // first (no stale tail) and keeps the capacity it already grew to.
        assert!(grid.row_text_into(0, &mut buf));
        assert_eq!(buf, "Hello");
        let cap_after_long = buf.capacity();
        assert!(grid.row_text_into(2, &mut buf)); // empty row -> clears the buffer
        assert_eq!(buf, "", "clear-then-append must drop the previous content");
        assert!(
            buf.capacity() >= cap_after_long,
            "capacity is retained across refills, not reallocated",
        );

        // Out-of-range leaves the buffer cleared and reports false.
        buf.push_str("stale");
        assert!(!grid.row_text_into(grid.rows(), &mut buf));
        assert_eq!(buf, "", "out-of-range row clears the buffer");
    }

    // =========================================================================
    // row_cols_into — per-COLUMN chars + fill (the erase-poof probe source)
    // =========================================================================

    #[test]
    fn test_row_cols_into_column_indexed_wide_and_fill() {
        let mut grid = Grid::new(4, 10);
        grid.write_char('A');
        grid.write_wide_char_styled(
            '\u{4E2D}', // wide CJK at cols 1-2
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.write_char('B'); // col 3
        let mut cols = Vec::new();
        let fill = grid.row_cols_into(0, &mut cols);
        // One char per COLUMN: lead char at its own column, '\0' at the wide
        // continuation, ' ' for blanks — column math survives CJK.
        assert_eq!(cols.len() as u16, grid.row(0).unwrap().len());
        assert_eq!(&cols[..4], &['A', '\u{4E2D}', '\0', 'B']);
        assert!(cols[4..].iter().all(|&c| c == ' '));
        assert_eq!(fill, 4, "fill = one past the last non-blank column");

        // Empty and out-of-range rows: cleared buffer, fill 0.
        assert_eq!(grid.row_cols_into(2, &mut cols), 0);
        assert!(cols.iter().all(|&c| c == ' '));
        cols.push('x');
        assert_eq!(grid.row_cols_into(9, &mut cols), 0);
        assert!(cols.is_empty(), "out-of-range clears the buffer");

        // Capacity is reused across refills (the zero-alloc probe contract).
        assert_eq!(grid.row_cols_into(0, &mut cols), 4);
        let cap = cols.capacity();
        assert_eq!(grid.row_cols_into(0, &mut cols), 4);
        assert_eq!(cols.capacity(), cap, "refill reuses the buffer");

        // The bounded sibling is byte-for-byte the corresponding prefix,
        // including a continuation at the cutoff, and clears on a zero limit.
        assert_eq!(grid.row_cols_prefix_into(0, 3, &mut cols), 3);
        assert_eq!(cols, ['A', '\u{4E2D}', '\0']);
        assert_eq!(grid.row_cols_prefix_into(0, 2, &mut cols), 2);
        assert_eq!(cols, ['A', '\u{4E2D}']);
        assert_eq!(grid.row_cols_prefix_into(0, 0, &mut cols), 0);
        assert!(cols.is_empty());
    }

    // =========================================================================
    // row_text — wide characters
    // =========================================================================

    #[test]
    fn test_row_text_wide_char() {
        let mut grid = Grid::new(4, 20);
        // Write a wide CJK character using the styled helper
        grid.write_wide_char_styled(
            '\u{4E2D}', // CJK "middle" (wide)
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        let text = grid.row_text(0).unwrap();
        assert!(
            text.contains('\u{4E2D}'),
            "wide char should appear in row_text, got: {text:?}"
        );
    }

    #[test]
    fn test_row_text_wide_char_continuation_skipped() {
        let mut grid = Grid::new(4, 20);
        grid.write_wide_char_styled(
            '\u{FF21}', // fullwidth A
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        let text = grid.row_text(0).unwrap();
        // The wide continuation cell should not produce an extra character
        let count = text.chars().filter(|&c| c == '\u{FF21}').count();
        assert_eq!(
            count, 1,
            "wide char should appear exactly once, got: {text:?}"
        );
    }

    #[test]
    fn test_row_text_mixed_wide_and_narrow() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('A');
        grid.write_wide_char_styled(
            '\u{4E2D}',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.write_char('B');
        let text = grid.row_text(0).unwrap();
        assert!(
            text.starts_with("A\u{4E2D}B"),
            "mixed narrow+wide+narrow, got: {text:?}"
        );
    }

    // =========================================================================
    // row_text — complex characters with combining marks
    // =========================================================================

    #[test]
    fn test_row_text_complex_with_combining() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('e');

        // Set up combining mark via extras
        let row = 0u16;
        let col = 0u16;
        if let Some(r) = grid.row_mut(row) {
            r.get_mut(col).unwrap().set_has_extras(true);
        }
        let extra = grid.extras_mut().get_or_create(CellCoord::new(row, col));
        extra.add_combining('\u{0301}'); // combining acute accent

        let text = grid.row_text(row).unwrap();
        assert!(
            text.contains("e\u{0301}"),
            "should contain base + combining, got: {text:?}"
        );
    }

    #[test]
    fn test_row_text_complex_emoji_via_extras() {
        let mut grid = Grid::new(4, 20);

        // First write a placeholder character so the row's len covers col 0.
        // Row::len() tracks the written extent; set_cell_complex_char alone
        // does not extend it.
        grid.write_char('X');
        grid.move_cursor_to(0, 1); // cursor past the cell we'll convert

        let row = 0u16;
        let col = 0u16;

        // Now convert that cell to a complex character.
        grid.set_cell_complex_char(row, col, "\u{1F44B}"); // 👋

        let text = grid.row_text(row).unwrap();
        assert!(
            text.contains("\u{1F44B}"),
            "complex char via set_cell_complex_char should appear, got: {text:?}"
        );
    }

    // =========================================================================
    // visible_content — all visible rows
    // =========================================================================

    #[test]
    fn test_visible_content_empty_grid() {
        let grid = Grid::new(3, 5);
        let content = grid.visible_content();
        // 3 rows, each ending with '\n'
        let lines: Vec<&str> = content.split('\n').collect();
        // split produces N+1 entries for N newlines, so 4 entries for 3 newlines
        assert_eq!(lines.len(), 4, "3 rows + trailing split");
        assert!(lines[3].is_empty(), "trailing element after last newline");
    }

    #[test]
    fn test_visible_content_with_text() {
        let mut grid = Grid::new(3, 10);
        write_text(&mut grid, "Row0");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "Row1");
        grid.move_cursor_to(2, 0);
        write_text(&mut grid, "Row2");

        let content = grid.visible_content();
        assert!(
            content.contains("Row0"),
            "should contain Row0, got: {content:?}"
        );
        assert!(content.contains("Row1"), "should contain Row1");
        assert!(content.contains("Row2"), "should contain Row2");
    }

    #[test]
    fn test_visible_content_line_count() {
        let grid = Grid::new(5, 10);
        let content = grid.visible_content();
        let newline_count = content.chars().filter(|&c| c == '\n').count();
        assert_eq!(
            newline_count, 5,
            "should have exactly 5 newlines for 5 rows"
        );
    }

    #[test]
    fn test_visible_content_preserves_row_order() {
        let mut grid = Grid::new(3, 10);
        write_text(&mut grid, "AAA");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "BBB");
        grid.move_cursor_to(2, 0);
        write_text(&mut grid, "CCC");

        let content = grid.visible_content();
        let lines: Vec<&str> = content.split('\n').collect();
        assert!(lines[0].starts_with("AAA"), "first row: {}", lines[0]);
        assert!(lines[1].starts_with("BBB"), "second row: {}", lines[1]);
        assert!(lines[2].starts_with("CCC"), "third row: {}", lines[2]);
    }

    #[test]
    fn test_visible_content_single_row_grid() {
        let mut grid = Grid::new(1, 10);
        write_text(&mut grid, "Only");
        let content = grid.visible_content();
        assert!(content.starts_with("Only"), "got: {content:?}");
        assert_eq!(content.chars().filter(|&c| c == '\n').count(), 1);
    }

    // =========================================================================
    // memory_used — basic sanity
    // =========================================================================

    #[test]
    fn test_memory_used_positive() {
        let grid = Grid::new(4, 20);
        assert!(grid.memory_used() > 0, "grid should report non-zero memory");
    }

    #[test]
    fn test_memory_used_larger_grid_uses_more() {
        let small = Grid::new(4, 20);
        let large = Grid::new(40, 200);
        assert!(
            large.memory_used() > small.memory_used(),
            "larger grid should use more memory: small={}, large={}",
            small.memory_used(),
            large.memory_used()
        );
    }

    #[test]
    fn test_memory_used_after_writing() {
        let empty = Grid::new(4, 20);
        let mut filled = Grid::new(4, 20);
        write_text(&mut filled, "Hello World");
        // Writing simple ASCII shouldn't significantly change memory
        // (rows are pre-allocated), but should not crash
        let _ = filled.memory_used();
        let _ = empty.memory_used();
    }

    // =========================================================================
    // Edge cases
    // =========================================================================

    #[test]
    fn test_row_text_last_visible_row() {
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(2, 0);
        write_text(&mut grid, "Last");
        let text = grid.row_text(2).unwrap();
        assert!(text.starts_with("Last"), "got: {text:?}");
    }

    #[test]
    fn test_row_text_after_overwrite() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "Hello");
        grid.move_cursor_to(0, 0);
        write_text(&mut grid, "World");
        let text = grid.row_text(0).unwrap();
        assert!(
            text.starts_with("World"),
            "overwritten text should show new content, got: {text:?}"
        );
    }

    #[test]
    fn test_resolved_char_after_overwrite() {
        let mut grid = Grid::new(4, 10);
        grid.write_char('A');
        grid.move_cursor_to(0, 0);
        grid.write_char('B');
        assert_eq!(grid.resolved_char(0, 0), Some('B'));
    }

    #[test]
    fn test_visible_content_with_wide_chars() {
        let mut grid = Grid::new(2, 10);
        grid.write_wide_char_styled(
            '\u{4E2D}',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        let content = grid.visible_content();
        assert!(
            content.contains('\u{4E2D}'),
            "visible_content should include wide char"
        );
    }

    #[test]
    fn test_row_text_minimum_grid() {
        // 1x1 grid edge case
        let mut grid = Grid::new(1, 1);
        grid.write_char('Z');
        let text = grid.row_text(0).unwrap();
        assert_eq!(text, "Z");
    }

    #[test]
    fn test_visible_content_minimum_grid() {
        let mut grid = Grid::new(1, 1);
        grid.write_char('Q');
        let content = grid.visible_content();
        assert!(content.starts_with('Q'), "got: {content:?}");
    }
}
