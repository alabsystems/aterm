// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for write_ascii_run_styled — the fast path for styled ASCII output.
//!
//! Covers content correctness, color/flag application, cursor position,
//! wrapping behavior, empty input, and consistency with write_char_wrap.
//!
//! Part of strategic verification phase, #5445 epic.

use super::*;

// ========================================================================
// write_ascii_run_styled content and styling correctness
// ========================================================================

#[test]
fn write_ascii_run_styled_content_matches_write_char() {
    let mut grid_styled = Grid::new(5, 10);
    let mut grid_char = Grid::new(5, 10);

    let fg = PackedColor::indexed(33);
    let bg = PackedColor::indexed(235);
    let flags = CellFlags::BOLD;
    let text = b"Hello!";
    let mut last_byte = None;

    grid_styled.write_ascii_run_styled(text, fg, bg, flags, &mut last_byte);

    for &byte in text {
        grid_char.write_char_styled(byte as char, fg, bg, flags);
    }

    for col in 0..text.len() as u16 {
        let styled_cell = grid_styled.cell(0, col).unwrap();
        let char_cell = grid_char.cell(0, col).unwrap();
        assert_eq!(
            styled_cell.char(),
            char_cell.char(),
            "char mismatch at col {col}",
        );
    }
}

#[test]
fn write_ascii_run_styled_applies_colors() {
    let mut grid = Grid::new(3, 10);
    let fg = PackedColor::indexed(196);
    let bg = PackedColor::indexed(17);
    let flags = CellFlags::empty();
    let mut last_byte = None;

    grid.write_ascii_run_styled(b"ABC", fg, bg, flags, &mut last_byte);

    for col in 0..3u16 {
        let cell = grid.cell(0, col).unwrap();
        let colors = cell.colors();
        assert!(colors.fg_is_indexed(), "col {col} fg should be indexed",);
        assert_eq!(colors.fg_index(), 196, "col {col} fg index mismatch",);
        assert!(colors.bg_is_indexed(), "col {col} bg should be indexed",);
        assert_eq!(colors.bg_index(), 17, "col {col} bg index mismatch",);
    }
}

#[test]
fn write_ascii_run_styled_applies_flags() {
    let mut grid = Grid::new(3, 10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    let flags = CellFlags::ITALIC | CellFlags::UNDERLINE;
    let mut last_byte = None;

    grid.write_ascii_run_styled(b"XYZ", fg, bg, flags, &mut last_byte);

    for col in 0..3u16 {
        let cell = grid.cell(0, col).unwrap();
        assert!(
            cell.flags().contains(CellFlags::ITALIC),
            "col {col} should have ITALIC",
        );
        assert!(
            cell.flags().contains(CellFlags::UNDERLINE),
            "col {col} should have UNDERLINE",
        );
    }
}

#[test]
fn write_ascii_run_styled_empty_input() {
    let mut grid = Grid::new(3, 10);
    let mut last_byte = None;
    let written = grid.write_ascii_run_styled(
        b"",
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
        &mut last_byte,
    );
    assert_eq!(written, 0);
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
    assert_eq!(last_byte, None);
}

#[test]
fn write_ascii_run_styled_single_byte() {
    let mut grid = Grid::new(3, 10);
    let fg = PackedColor::indexed(42);
    let bg = PackedColor::DEFAULT_BG;
    let mut last_byte = None;
    let written = grid.write_ascii_run_styled(b"Q", fg, bg, CellFlags::BOLD, &mut last_byte);
    assert_eq!(written, 1);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'Q');
    assert_eq!(grid.cursor_col(), 1);
    assert_eq!(last_byte, Some(b'Q'));
}

#[test]
fn write_ascii_run_styled_fills_exact_line_with_deferred_wrap() {
    let mut grid = Grid::new(3, 5);
    let fg = PackedColor::indexed(10);
    let bg = PackedColor::indexed(20);
    let mut last_byte = None;

    grid.write_ascii_run_styled(b"ABCDE", fg, bg, CellFlags::empty(), &mut last_byte);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 4).unwrap().char(), 'E');
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 4);
    assert!(grid.pending_wrap());
    assert_eq!(last_byte, Some(b'E'));
}

#[test]
fn write_ascii_run_styled_wraps_across_lines() {
    let mut grid = Grid::new(3, 4);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    let mut last_byte = None;

    let written =
        grid.write_ascii_run_styled(b"ABCDEFGH", fg, bg, CellFlags::empty(), &mut last_byte);
    assert_eq!(written, 8);

    // First line
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 3).unwrap().char(), 'D');
    // Second line
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'E');
    assert_eq!(grid.cell(1, 3).unwrap().char(), 'H');

    assert!(grid.row(1).unwrap().is_wrapped());
    assert_eq!(last_byte, Some(b'H'));
}

#[test]
fn write_ascii_run_styled_at_cursor_offset() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 7);
    let fg = PackedColor::indexed(99);
    let bg = PackedColor::DEFAULT_BG;
    let mut last_byte = None;

    let written = grid.write_ascii_run_styled(b"XYZ", fg, bg, CellFlags::empty(), &mut last_byte);
    assert_eq!(written, 3);

    // Content at offset
    assert_eq!(grid.cell(0, 7).unwrap().char(), 'X');
    assert_eq!(grid.cell(0, 8).unwrap().char(), 'Y');
    assert_eq!(grid.cell(0, 9).unwrap().char(), 'Z');
    // Preceding cells untouched
    assert!(grid.cell(0, 0).unwrap().is_empty());
    assert!(grid.cell(0, 6).unwrap().is_empty());

    // Deferred wrap at end of line
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 9);
    assert!(grid.pending_wrap());
}

#[test]
fn write_ascii_run_styled_last_byte_tracks_correctly() {
    let mut grid = Grid::new(3, 10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    let mut last_byte = Some(b'Z');

    grid.write_ascii_run_styled(b"ABC", fg, bg, CellFlags::empty(), &mut last_byte);
    assert_eq!(last_byte, Some(b'C'));
}

#[test]
fn write_ascii_run_styled_scrolls_at_screen_bottom() {
    let mut grid = Grid::new(2, 3);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    let mut last_byte = None;

    let written =
        grid.write_ascii_run_styled(b"ABCDEF", fg, bg, CellFlags::empty(), &mut last_byte);
    assert_eq!(written, 6);

    // Deferred wrap — cursor at last col of row 1
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 2);
    assert!(grid.pending_wrap());

    // Resolve deferred wrap → triggers scroll
    grid.resolve_pending_wrap();
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 0);
    // After scroll, row 0 has second batch content
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'E');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'F');
}
