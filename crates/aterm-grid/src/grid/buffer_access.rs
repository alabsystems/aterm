// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! [`BufferAccess`] implementation for [`Grid`].
//!
//! Provides the unified read-only buffer access interface so navigation
//! logic (vi mode, emacs mode, editor) can operate on the terminal grid
//! through `dyn BufferAccess` without coupling to grid internals.
//!
//! This is the sole buffer-reading interface: vi mode, emacs mode, and editor
//! all operate through `dyn BufferAccess` from `aterm-types`.

use aterm_types::BufferAccess;

use super::Grid;

impl BufferAccess for Grid {
    fn char_at(&self, line: i32, col: u16) -> Option<char> {
        if line < 0 {
            return scrollback_resolved_char(self, line, col);
        }
        let row = line_to_visible_row(line)?;
        self.resolved_char(row, col)
    }

    fn line_len(&self, line: i32) -> u16 {
        if line < 0 {
            return scrollback_row(self, line).map_or(0, |row| row.len());
        }
        let Some(row) = line_to_visible_row(line) else {
            return 0;
        };
        self.row(row).map_or(0, crate::row::Row::len)
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

    fn line_text(&self, line: i32) -> Option<String> {
        if line < 0 {
            return scrollback_row(self, line).map(|row| {
                row.cells
                    .iter()
                    .enumerate()
                    .map(|(col, cell)| {
                        if cell.is_complex() {
                            #[allow(clippy::cast_possible_truncation)]
                            row.get_extra(col as u16)
                                .and_then(|e| e.complex_char())
                                .and_then(|s| s.chars().next())
                                .unwrap_or('\u{FFFD}')
                        } else {
                            cell.char()
                        }
                    })
                    .collect()
            });
        }
        let row = line_to_visible_row(line)?;
        self.row_text(row)
    }

    fn is_wide(&self, line: i32, col: u16) -> bool {
        if line < 0 {
            return scrollback_row(self, line)
                .and_then(|row| row.cells.get(col as usize).map(crate::Cell::is_wide))
                .unwrap_or(false);
        }
        let Some(row) = line_to_visible_row(line) else {
            return false;
        };
        self.cell(row, col).is_some_and(crate::Cell::is_wide)
    }

    fn display_offset(&self) -> i32 {
        i32::try_from(Grid::display_offset(self)).unwrap_or(0)
    }

    fn is_line_wrapped(&self, line: i32) -> bool {
        if line < 0 {
            let rev_idx = match usize::try_from(-(i64::from(line)) - 1) {
                Ok(idx) => idx,
                Err(_) => return false,
            };
            return self
                .history_line_rev(rev_idx)
                .is_some_and(|l| l.is_wrapped());
        }
        let Some(row_idx) = line_to_visible_row(line) else {
            return false;
        };
        self.row(row_idx).is_some_and(crate::row::Row::is_wrapped)
    }
}

/// Convert a logical line to a grid visible-row index.
///
/// Returns `Some(row)` for non-negative lines within `u16` range.
/// Negative lines (scrollback) are handled inline by each trait method.
fn line_to_visible_row(line: i32) -> Option<u16> {
    u16::try_from(line).ok()
}

/// Materialize a scrollback row from a negative line index.
///
/// Line -1 is the most recent scrollback line, -2 the next, etc.
fn scrollback_row(grid: &Grid, line: i32) -> Option<super::scroll_materialize::MaterializedRow> {
    let rev_idx = usize::try_from(-(i64::from(line)) - 1).ok()?;
    grid.materialize_scrollback_row_full(rev_idx, grid.cols())
}

/// Resolve a character from a scrollback row, checking overflow.
///
/// Like `Grid::resolved_char()` but for scrollback rows where extras
/// are stored in `MaterializedRow` instead of the grid-level `CellExtras`.
fn scrollback_resolved_char(grid: &Grid, line: i32, col: u16) -> Option<char> {
    let row = scrollback_row(grid, line)?;
    let cell = row.cells.get(col as usize)?;
    if cell.is_complex() {
        row.get_extra(col)
            .and_then(|e| e.complex_char())
            .and_then(|s| s.chars().next())
            .or(Some('\u{FFFD}'))
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
    fn test_char_at_visible() {
        let grid = make_grid();
        assert_eq!(grid.char_at(0, 0), Some('H'));
        assert_eq!(grid.char_at(0, 1), Some('i'));
    }

    #[test]
    fn test_char_at_empty_cell() {
        let grid = make_grid();
        assert_eq!(grid.char_at(0, 5), Some(' '));
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
    fn test_visible_rows_and_cols() {
        let grid = make_grid();
        assert_eq!(BufferAccess::visible_rows(&grid), 3);
        assert_eq!(BufferAccess::cols(&grid), 10);
    }

    #[test]
    fn test_line_text() {
        let grid = make_grid();
        let text = grid.line_text(0).expect("should have text for row 0");
        assert!(text.starts_with("Hi"));
        assert_eq!(grid.line_text(-1), None); // no scrollback
    }

    #[test]
    fn test_display_offset_default() {
        let grid = make_grid();
        assert_eq!(BufferAccess::display_offset(&grid), 0);
    }

    #[test]
    fn test_is_wide_default() {
        let grid = make_grid();
        assert!(!grid.is_wide(0, 0));
    }

    // --- Scrollback tests (#5613) ---

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

    // --- Supplementary plane character resolution tests (#5939) ---

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
}
