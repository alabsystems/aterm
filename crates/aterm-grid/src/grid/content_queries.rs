// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Derived-content queries on [`Grid`]: text extraction and memory accounting.

use super::Grid;
use crate::CellCoord;

impl Grid {
    /// The resolved character at a grid position.
    #[must_use]
    #[inline]
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
        total = total.saturating_add(self.storage.extras.memory_used());
        total = total.saturating_add(
            self.storage
                .rows
                .capacity()
                .saturating_mul(std::mem::size_of::<crate::row::Row>()),
        );
        total = total.saturating_add(
            self.storage
                .tab_stops
                .capacity()
                .saturating_mul(std::mem::size_of::<bool>()),
        );
        total = total.saturating_add(
            self.storage
                .ring_extras
                .capacity()
                .saturating_mul(std::mem::size_of::<super::ring_extras::RingSlot>()),
        );
        total = total.saturating_add(self.storage.ring_extras.retained_bytes());
        if let Some(undo) = &self.storage.resize_undo {
            use super::resize_undo::{ResizeUndo, UndoRow};
            total = total
                .saturating_add(std::mem::size_of::<ResizeUndo>())
                .saturating_add(
                    undo.entries
                        .capacity()
                        .saturating_mul(std::mem::size_of::<UndoRow>()),
                );
            for entry in &undo.entries {
                if let UndoRow::Demoted(row, extras) | UndoRow::Pushed(row, extras) = entry {
                    total = total.saturating_add(row.heap_memory_used());
                    if let Some(extras) = extras {
                        total = total.saturating_add(extras.retained_memory_used());
                    }
                }
            }
        }
        total = total.saturating_add(self.storage.lazy_buffer.memory_used());
        if let Some(scrollback) = &self.storage.scrollback {
            total = total.saturating_add(scrollback.total_memory_used());
        }
        total
    }

    /// The text of a visible row ([`row_text_into`](Self::row_text_into)).
    #[must_use]
    pub fn row_text(&self, row: u16) -> Option<String> {
        let mut s = String::new();
        self.row_text_into(row, &mut s).then_some(s)
    }

    /// Write visible row `row`'s resolved text into `out` ([`row_text`](Self::row_text)).
    #[must_use]
    pub fn row_text_into(&self, row: u16, out: &mut String) -> bool {
        out.clear();
        if row >= self.rows() {
            return false;
        }
        let view = self.visible_row_view(row);
        let len = view.written_len();
        out.reserve(len as usize);
        for col in 0..len {
            let Some(cell) = view.cell(col) else {
                continue;
            };
            if view.is_wide_continuation(col) {
                continue;
            }
            view.push_cell_text(col, cell, out);
        }
        true
    }

    /// Fill `out` with visible row `row` as one `char` per column and return the row's fill.
    #[inline]
    pub fn row_cols_into(&self, row: u16, out: &mut Vec<char>) -> u16 {
        self.row_cols_prefix_into(row, usize::MAX, out)
    }

    /// Fill `out` with at most the first `prefix_len` columns of visible row `row`
    /// ([`row_cols_into`](Self::row_cols_into)).
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
            let ch = view.col_char(col);
            out.push(ch);
            if ch != ' ' {
                fill = col + 1;
            }
        }
        fill
    }

    /// A live screen row ([`row_text_screen`](Self::row_text_screen)).
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "flat SGR tracking per cell")]
    pub fn row_ansi_text_screen(&self, row: u16) -> Option<String> {
        use crate::CellFlags;

        let r = self.storage.row_at_screen(row)?;
        let mut s = String::with_capacity(r.len() as usize * 2);
        let extras_empty = self.storage.extras.is_empty();

        let mut cur_bold = false;
        let mut cur_dim = false;
        let mut cur_italic = false;
        let mut cur_underline: Option<&str> = None;
        let mut cur_underline_color: Option<u32> = None;
        let mut cur_blink = false;
        let mut cur_inverse = false;
        let mut cur_hidden = false;
        let mut cur_strikethrough = false;
        let mut cur_overline_or_script: Option<&str> = None;
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

            if r.is_cell_wide_continuation(col) {
                continue;
            }

            let flags = cell.flags();
            let colors = cell.colors();

            let want_bold = flags.contains(CellFlags::BOLD);
            let want_dim = flags.contains(CellFlags::DIM);
            let want_italic = flags.contains(CellFlags::ITALIC);
            let want_underline = flags.underline_sgr();
            let want_blink = flags.contains(CellFlags::BLINK);
            let want_inverse = flags.contains(CellFlags::INVERSE);
            let want_hidden = flags.contains(CellFlags::HIDDEN);
            let want_strikethrough = flags.contains(CellFlags::STRIKETHROUGH);
            let want_overline_or_script = flags.overline_or_script_sgr();
            let want_underline_color = if cell.has_extras() && !extras_empty {
                self.storage
                    .extras
                    .get(CellCoord::new(row, col))
                    .and_then(crate::CellExtra::underline_color_u32)
            } else {
                None
            };

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

            let changed = want_bold != cur_bold
                || want_dim != cur_dim
                || want_italic != cur_italic
                || want_underline != cur_underline
                || want_blink != cur_blink
                || want_inverse != cur_inverse
                || want_hidden != cur_hidden
                || want_strikethrough != cur_strikethrough
                || want_overline_or_script != cur_overline_or_script
                || want_underline_color != cur_underline_color
                || want_fg_indexed != cur_fg_indexed
                || want_bg_indexed != cur_bg_indexed
                || want_fg_rgb != cur_fg_rgb
                || want_bg_rgb != cur_bg_rgb;

            if changed {
                let has_attrs = want_bold
                    || want_dim
                    || want_italic
                    || want_underline.is_some()
                    || want_blink
                    || want_inverse
                    || want_hidden
                    || want_strikethrough
                    || want_overline_or_script.is_some()
                    || want_underline_color.is_some()
                    || want_fg_indexed.is_some()
                    || want_bg_indexed.is_some()
                    || want_fg_rgb.is_some()
                    || want_bg_rgb.is_some();

                if has_attrs {
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
                    if let Some(underline) = want_underline {
                        params.push(underline.to_string());
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
                    if let Some(line) = want_overline_or_script {
                        params.push(line.to_string());
                    }
                    if let Some(color) = want_underline_color {
                        params.push(crate::underline_color_sgr(color));
                    }
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
                cur_overline_or_script = want_overline_or_script;
                cur_underline_color = want_underline_color;
                cur_fg_indexed = want_fg_indexed;
                cur_bg_indexed = want_bg_indexed;
                cur_fg_rgb = want_fg_rgb;
                cur_bg_rgb = want_bg_rgb;
            }

            if cell.is_complex() {
                if !self.push_complex_char_at(row, col, &mut s) {
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

    #[test]
    fn text_reads_follow_the_cells() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "Hello");
        grid.move_cursor_to(0, 0);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(1, 3);
        grid.write_char('X');
        grid.move_cursor_to(2, 0);
        grid.write_char('a');
        grid.write_wide_char_styled(
            '\u{4E2D}',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.write_char('b');

        assert_eq!(grid.row_text(0).as_deref(), Some("ABCDE"));
        assert_eq!(grid.row_text(1).as_deref(), Some("   X"));
        assert_eq!(grid.row_text(2).as_deref(), Some("a\u{4E2D}b"));
        assert_eq!(grid.row_text(3).as_deref(), Some(""));
        assert_eq!(grid.row_text(4), None);
        assert_eq!(grid.visible_content(), "ABCDE\n   X\na\u{4E2D}b\n\n");

        assert_eq!(grid.resolved_char(0, 0), Some('A'));
        assert_eq!(grid.resolved_char(2, 1), Some('\u{4E2D}'));
        assert_eq!(grid.resolved_char(3, 0), Some(' '));
        assert_eq!(grid.resolved_char(9, 0), None);
        assert_eq!(grid.resolved_char(0, 9), None);
    }

    #[test]
    fn test_row_text_into_matches_row_text_and_reuses_capacity() {
        let mut grid = Grid::new(4, 20);
        write_text(&mut grid, "Hello");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "World!");

        let mut buf = String::new();
        for row in 0..grid.rows() {
            let owned = grid.row_text(row);
            let ok = grid.row_text_into(row, &mut buf);
            assert!(ok, "in-bounds row must succeed");
            assert_eq!(Some(buf.as_str()), owned.as_deref(), "row {row} parity");
        }

        assert!(grid.row_text_into(0, &mut buf));
        assert_eq!(buf, "Hello");
        let cap_after_long = buf.capacity();
        assert!(grid.row_text_into(2, &mut buf)); // empty row -> clears the buffer
        assert_eq!(buf, "", "clear-then-append must drop the previous content");
        assert!(
            buf.capacity() >= cap_after_long,
            "capacity is retained across refills, not reallocated",
        );

        buf.push_str("stale");
        assert!(!grid.row_text_into(grid.rows(), &mut buf));
        assert_eq!(buf, "", "out-of-range row clears the buffer");
    }

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
        assert_eq!(cols.len() as u16, grid.row(0).unwrap().len());
        assert_eq!(&cols[..4], &['A', '\u{4E2D}', '\0', 'B']);
        assert!(cols[4..].iter().all(|&c| c == ' '));
        assert_eq!(fill, 4, "fill = one past the last non-blank column");

        assert_eq!(grid.row_cols_into(2, &mut cols), 0);
        assert!(cols.iter().all(|&c| c == ' '));
        cols.push('x');
        assert_eq!(grid.row_cols_into(9, &mut cols), 0);
        assert!(cols.is_empty(), "out-of-range clears the buffer");

        assert_eq!(grid.row_cols_into(0, &mut cols), 4);
        let cap = cols.capacity();
        assert_eq!(grid.row_cols_into(0, &mut cols), 4);
        assert_eq!(cols.capacity(), cap, "refill reuses the buffer");

        assert_eq!(grid.row_cols_prefix_into(0, 3, &mut cols), 3);
        assert_eq!(cols, ['A', '\u{4E2D}', '\0']);
        assert_eq!(grid.row_cols_prefix_into(0, 2, &mut cols), 2);
        assert_eq!(cols, ['A', '\u{4E2D}']);
        assert_eq!(grid.row_cols_prefix_into(0, 0, &mut cols), 0);
        assert!(cols.is_empty());
    }

    #[test]
    fn test_row_text_complex_with_combining() {
        let mut grid = Grid::new(4, 20);
        grid.write_char('e');

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

        grid.write_char('X');
        grid.move_cursor_to(0, 1); // cursor past the cell we'll convert

        let row = 0u16;
        let col = 0u16;

        grid.set_cell_complex_char(row, col, "\u{1F44B}"); // 👋

        let text = grid.row_text(row).unwrap();
        assert!(
            text.contains("\u{1F44B}"),
            "complex char via set_cell_complex_char should appear, got: {text:?}"
        );
    }

    #[test]
    fn memory_used_counts_resize_undo_cells_and_retained_extras() {
        use std::sync::Arc;

        use crate::{ImageData, ImageFormat, ImageRef};

        for (cursor_row, retained_row) in [(3, 0), (0, 3)] {
            let mut grid = Grid::with_scrollback(4, 20, 0);
            for row in 0..4 {
                grid.set_cursor(row, 0);
                grid.write_char('x');
            }
            let image = Arc::new(ImageData {
                bytes: vec![7; 64 * 1024],
                format: ImageFormat::Png,
                rows: 1,
                cols: 1,
                z_index: 0,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: crate::ImageScaling::Fit,
                source_rect: None,
            });
            let payload_bytes = image.bytes.len();
            grid.set_cell_image(
                retained_row,
                0,
                ImageRef {
                    image,
                    cell_row: 0,
                    cell_col: 0,
                    kitty: None,
                },
            );
            let url: Arc<str> = Arc::from("u".repeat(4096));
            grid.extras_mut()
                .get_or_create(CellCoord::new(retained_row, 0))
                .set_hyperlink(Some(url));
            grid.set_cursor(cursor_row, 0);
            grid.resize_no_reflow(3, 20);
            assert_eq!(grid.resize_undo_rows(), 1);

            let with_undo = grid.memory_used();
            let pages = grid.storage.pages.total_memory();
            grid.drop_resize_undo();
            assert_eq!(grid.storage.pages.total_memory(), pages);
            let released = with_undo - grid.memory_used();
            assert!(
                released >= payload_bytes + 4096 + 20 * std::mem::size_of::<crate::Cell>(),
                "undo accounting omitted its image, hyperlink or cell copy: {released} bytes"
            );
        }
    }

    #[test]
    fn memory_used_counts_blank_resize_undo_capacity() {
        use super::super::resize_undo::{ResizeUndo, UndoRow};

        let mut grid = Grid::with_scrollback(8, 20, 0);
        grid.resize_no_reflow(4, 20);
        let undo = grid.storage.resize_undo.as_ref().expect("blank trim stash");
        assert!(
            undo.entries
                .iter()
                .all(|entry| matches!(entry, UndoRow::Trimmed))
        );
        let expected = std::mem::size_of::<ResizeUndo>()
            + undo.entries.capacity() * std::mem::size_of::<UndoRow>();
        let with_undo = grid.memory_used();
        grid.drop_resize_undo();
        assert_eq!(with_undo - grid.memory_used(), expected);
    }
}
