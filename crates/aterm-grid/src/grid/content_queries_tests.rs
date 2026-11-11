// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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
