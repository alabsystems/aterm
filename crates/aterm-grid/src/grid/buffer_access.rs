// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! [`BufferAccess`] for [`Grid`]: the read-only interface through which vi
//! mode, emacs mode and the editor navigate the grid (`dyn BufferAccess`).
//!
//! A navigator's line is live-relative: line 0 is the top of the LIVE screen
//! whatever the display offset, and a negative line counts back into history
//! from there (-1 is the newest history line). Vi mode places its cursor from
//! the terminal cursor's row and the window draws it at `line +
//! display_offset`, so a scrolled-back viewport must not move what a line
//! names. Every read resolves its line once, in [`line_view`], and reads the
//! row it names through that one view: the live arm is
//! [`Grid::screen_row_view`] (cells and extras both keyed on the live screen
//! row), never the viewport's [`Grid::row`].

use std::sync::Arc;

use aterm_types::BufferAccess;

use super::{Grid, VisibleRowView};

impl BufferAccess for Grid {
    fn char_at(&self, line: i32, col: u16) -> Option<char> {
        view_char(&line_view(self, line), col)
    }

    fn line_len(&self, line: i32) -> u16 {
        line_view(self, line).len()
    }

    fn total_lines(&self) -> i32 {
        let scrollback = self.storage.scrollback_lines();
        i32::try_from(scrollback).unwrap_or(i32::MAX)
    }

    fn visible_rows(&self) -> u16 {
        self.rows()
    }

    fn cols(&self) -> u16 {
        self.cols()
    }

    /// A live line's text is its row text (wide continuations skipped,
    /// complex strings and combining marks included); a history line's is one
    /// char per cell resolved as [`char_at`](BufferAccess::char_at) resolves
    /// it, which vi's scrollback line caches index by column.
    fn line_text(&self, line: i32) -> Option<String> {
        let view = line_view(self, line);
        match &view {
            VisibleRowView::Empty => None,
            VisibleRowView::Live { screen_row, .. } => self.row_text_screen(*screen_row),
            VisibleRowView::History { mat } => {
                let cells = u16::try_from(mat.cells.len()).unwrap_or(u16::MAX);
                Some((0..cells).filter_map(|col| view_char(&view, col)).collect())
            }
        }
    }

    /// One row resolution for the whole line, each cell judged against its
    /// left neighbour ([`VisibleRowView::is_wide_continuation`]), the same rule
    /// in both tiers: a DECSCA-protected cell shares bit 10 with
    /// `WIDE_CONTINUATION` and is not one.
    fn line_wide_continuations(&self, line: i32) -> Option<Vec<bool>> {
        let view = line_view(self, line);
        if matches!(view, VisibleRowView::Empty) {
            return None;
        }
        Some(
            (0..self.cols())
                .map(|col| view.is_wide_continuation(col))
                .collect(),
        )
    }

    fn is_wide(&self, line: i32, col: u16) -> bool {
        line_view(self, line)
            .cell(col)
            .is_some_and(|cell| cell.is_wide())
    }

    fn display_offset(&self) -> i32 {
        i32::try_from(Grid::display_offset(self)).unwrap_or(0)
    }

    fn is_line_wrapped(&self, line: i32) -> bool {
        line_view(self, line).is_wrapped()
    }
}

/// The row a navigator's `line` names, resolved live-relative: a line in
/// `0..rows` is that row of the live screen ([`Grid::screen_row_view`],
/// whatever the display offset), and line `-n` is the `n`th newest history
/// line, materialized with its extras. Anything else is
/// [`VisibleRowView::Empty`].
fn line_view(grid: &Grid, line: i32) -> VisibleRowView<'_> {
    if line >= 0 {
        return u16::try_from(line).map_or(VisibleRowView::Empty, |row| grid.screen_row_view(row));
    }
    // -1 is the newest history line (reverse index 0). Widened to i64 so
    // i32::MIN negates.
    usize::try_from(-i64::from(line) - 1)
        .ok()
        .and_then(|rev_idx| grid.materialize_scrollback_row_full(rev_idx, grid.cols()))
        .map_or(VisibleRowView::Empty, |mat| VisibleRowView::History {
            mat: Arc::new(mat),
        })
}

/// The character at `col` of a resolved line: a complex cell's base codepoint
/// from the extras of the tier the row came from (U+FFFD if they are gone),
/// otherwise the cell's own char. `None` past the row.
fn view_char(view: &VisibleRowView<'_>, col: u16) -> Option<char> {
    let cell = view.cell(col)?;
    if cell.is_complex() {
        Some(
            view.cell_data(col, cell)
                .complex_base()
                .unwrap_or('\u{FFFD}'),
        )
    } else {
        Some(cell.char())
    }
}

#[cfg(test)]
mod tests {
    use aterm_types::BufferAccess;

    use crate::grid::Grid;

    fn make_grid() -> Grid {
        let mut grid = Grid::new(3, 10);
        grid.write_char('H');
        grid.write_char('i');
        grid
    }

    fn write_line(grid: &mut Grid, text: &str) {
        grid.carriage_return();
        for ch in text.chars() {
            grid.write_char(ch);
        }
    }

    #[test]
    fn test_char_at_negative_line_no_scrollback() {
        let grid = make_grid();
        assert_eq!(grid.char_at(-1, 0), None);
    }

    #[test]
    fn test_char_at_out_of_bounds_row() {
        let grid = make_grid();
        assert_eq!(grid.char_at(100, 0), None);
    }

    #[test]
    fn test_line_len() {
        let grid = make_grid();
        assert_eq!(grid.line_len(0), 2);
        assert_eq!(grid.line_len(1), 0);
        assert_eq!(grid.line_len(-1), 0); // no scrollback → 0
    }

    #[test]
    fn test_line_text() {
        let grid = make_grid();
        let text = grid.line_text(0).expect("should have text for row 0");
        assert!(text.starts_with("Hi"));
        assert_eq!(grid.line_text(-1), None); // no scrollback
    }

    // --- Scrollback tests ---

    #[test]
    fn test_char_at_reads_scrollback() {
        let mut grid = Grid::with_scrollback(2, 4, 4);
        write_line(&mut grid, "AB");
        grid.line_feed();
        write_line(&mut grid, "CD");
        grid.line_feed();
        write_line(&mut grid, "EF");

        assert_eq!(grid.scrollback_lines(), 1);
        assert_eq!(grid.char_at(-1, 0), Some('A'));
        assert_eq!(grid.char_at(-1, 1), Some('B'));
    }

    #[test]
    fn test_line_len_reads_scrollback() {
        // 2-row grid needs 3 writes + 2 LFs to push first line into scrollback.
        let mut grid = Grid::with_scrollback(2, 4, 4);
        write_line(&mut grid, "XYZ");
        grid.line_feed();
        write_line(&mut grid, "A");
        grid.line_feed();
        write_line(&mut grid, "B");

        assert_eq!(grid.scrollback_lines(), 1);
        assert_eq!(grid.line_len(-1), 3);
    }

    #[test]
    fn test_line_text_reads_scrollback() {
        // 2-row grid needs 3 writes + 2 LFs to push first line into scrollback.
        let mut grid = Grid::with_scrollback(2, 4, 4);
        write_line(&mut grid, "Hi");
        grid.line_feed();
        write_line(&mut grid, "Z");
        grid.line_feed();
        write_line(&mut grid, "W");

        assert_eq!(grid.scrollback_lines(), 1);
        let text = grid.line_text(-1).expect("scrollback text should exist");
        assert!(text.starts_with("Hi"), "expected 'Hi...' but got '{text}'");
    }

    #[test]
    fn test_is_wide_reads_scrollback() {
        let mut grid = Grid::with_scrollback(1, 4, 4);
        write_line(&mut grid, "中");
        grid.line_feed();
        write_line(&mut grid, "A");

        assert_eq!(grid.scrollback_lines(), 1);
        assert!(grid.is_wide(-1, 0));
    }

    #[test]
    fn test_scrollback_out_of_range() {
        // 2-row grid needs 3 writes + 2 LFs to push first line into scrollback.
        let mut grid = Grid::with_scrollback(2, 4, 4);
        write_line(&mut grid, "AB");
        grid.line_feed();
        write_line(&mut grid, "CD");
        grid.line_feed();
        write_line(&mut grid, "EF");

        assert_eq!(grid.scrollback_lines(), 1);
        assert_eq!(grid.char_at(-2, 0), None);
        assert_eq!(grid.line_len(-2), 0);
        assert_eq!(grid.line_text(-2), None);
    }

    // --- Supplementary plane character resolution tests ---

    #[test]
    fn test_resolved_char_supplementary_plane_emoji() {
        let mut grid = Grid::new(3, 10);
        grid.write_char('\u{1F600}'); // grinning face (supplementary plane)
        assert_eq!(grid.resolved_char(0, 0), Some('\u{1F600}'));
    }

    #[test]
    fn test_char_at_resolves_supplementary_plane() {
        let mut grid = Grid::new(3, 10);
        grid.write_char('\u{1F680}'); // rocket emoji
        assert_eq!(grid.char_at(0, 0), Some('\u{1F680}'));
    }

    #[test]
    fn test_resolved_char_bmp_unchanged() {
        let mut grid = Grid::new(3, 10);
        grid.write_char('A');
        grid.write_char('\u{4E2D}'); // CJK character (BMP)
        assert_eq!(grid.resolved_char(0, 0), Some('A'));
        assert_eq!(grid.resolved_char(0, 1), Some('\u{4E2D}'));
    }

    #[test]
    fn test_line_text_includes_supplementary_plane() {
        let mut grid = Grid::new(3, 10);
        grid.write_char('a');
        grid.write_char('\u{1F4A9}'); // pile of poo emoji
        grid.write_char('b');
        let text = grid.line_text(0).expect("row 0 text");
        assert!(
            text.starts_with("a\u{1F4A9}"),
            "expected line text starting with 'a💩' but got '{text}'"
        );
    }

    #[test]
    fn test_row_text_includes_supplementary_plane() {
        let mut grid = Grid::new(3, 10);
        grid.write_char('\u{1F600}'); // grinning face
        grid.write_char('x');
        let text = grid.row_text(0).expect("row 0 text");
        assert!(
            text.starts_with("\u{1F600}x"),
            "expected row_text starting with '😀x' but got '{text}'"
        );
    }

    /// Every read a navigator makes of one line: `char_at` and `is_wide` per
    /// column, `line_len`, `line_text`, `line_wide_continuations` and
    /// `is_line_wrapped`.
    type LineReads = (
        Vec<Option<char>>,
        Vec<bool>,
        u16,
        Option<String>,
        Option<Vec<bool>>,
        bool,
    );

    fn line_reads(grid: &Grid, line: i32) -> LineReads {
        let cols = grid.cols();
        (
            (0..cols).map(|c| grid.char_at(line, c)).collect(),
            (0..cols).map(|c| grid.is_wide(line, c)).collect(),
            grid.line_len(line),
            grid.line_text(line),
            grid.line_wide_continuations(line),
            grid.is_line_wrapped(line),
        )
    }

    /// Lay `text` on the cursor's row from column 0, `日`/`本` over two cells
    /// (lead + continuation, as the VT writer stores them), then flag the row
    /// as a soft-wrap continuation of the one above when `wrapped`.
    fn put_row(grid: &mut Grid, text: &str, wrapped: bool) {
        use crate::{CellFlags, PackedColors};
        grid.carriage_return();
        for ch in text.chars() {
            let at = grid.cursor();
            if matches!(ch, '日' | '本') {
                assert!(grid.write_wide_char_at_cursor_packed(
                    ch,
                    PackedColors::DEFAULT,
                    CellFlags::empty()
                ));
                grid.move_cursor_to(at.row, at.col + 2);
            } else {
                grid.write_char(ch);
            }
        }
        let row = grid.cursor().row;
        grid.row_mut(row).expect("cursor row").set_wrapped(wrapped);
    }

    /// LINE 0 IS THE TOP OF THE LIVE SCREEN, WHEREVER THE VIEWPORT IS. Vi
    /// mode's lines do not move with the display offset: a negative line is
    /// history counted back from the live screen, and the window draws the vi
    /// cursor at `line + display_offset`. Every `line >= 0` read went through
    /// the viewport's row lookup instead, so scrolled back three rows, `w` on
    /// live line 0 read the history line three rows above it and landed in
    /// the middle of a word. Every read of every line must answer the same at
    /// every display offset.
    #[test]
    fn every_read_answers_the_live_screen_at_any_display_offset() {
        let mut grid = Grid::with_scrollback(4, 8, 64);
        let rows = [
            ("a b", false),
            ("cc dd", false),
            ("日本 x", false),
            ("eee f", false),
            ("gggggggg", false),
            ("gg h", true),
            ("日 i jj", false),
        ];
        for (i, (text, wrapped)) in rows.iter().enumerate() {
            if i > 0 {
                grid.line_feed();
            }
            put_row(&mut grid, text, *wrapped);
        }
        let history = i32::try_from(grid.scrollback_lines()).expect("small history");
        assert_eq!(
            history, 3,
            "the fixture leaves three rows to scroll back over"
        );
        let all: Vec<i32> = (-history..i32::from(grid.rows())).collect();
        let live: Vec<LineReads> = all.iter().map(|&l| line_reads(&grid, l)).collect();
        // Non-vacuous: the live screen holds a wide glyph and a soft-wrapped row,
        // so every per-row flag is read on a row where it is set.
        let on_screen = &live[all.iter().position(|&l| l == 0).expect("line 0")..];
        assert!(
            on_screen.iter().any(|r| r.1.iter().any(|w| *w)),
            "no wide glyph on screen"
        );
        assert!(
            on_screen.iter().any(|r| r.5),
            "no soft-wrapped row on screen"
        );
        assert_eq!(
            grid.line_text(0).as_deref(),
            Some("eee f"),
            "line 0 is the live top row"
        );
        assert_eq!(
            grid.char_at(-1, 0),
            Some('日'),
            "line -1 is the newest history row"
        );
        for offset in 1..=history {
            grid.scroll_display(1);
            assert_eq!(Grid::display_offset(&grid), offset as usize);
            for (&line, want) in all.iter().zip(&live) {
                assert_eq!(
                    &line_reads(&grid, line),
                    want,
                    "line {line} read differently at display offset {offset}"
                );
            }
        }
    }
}
