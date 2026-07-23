// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Basic grid operations — construction, cursor movement, write, scroll,
//! resize, erase, insert/delete chars/lines, screen alignment.

use super::super::*;

#[test]
fn grid_new() {
    let grid = Grid::new(24, 80);
    assert_eq!(grid.rows(), 24);
    assert_eq!(grid.cols(), 80);
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_assert_invariants_on_new() {
    let grid = Grid::new(24, 80);
    grid.assert_invariants();
}

#[test]
fn grid_assert_invariants_after_operations() {
    let mut grid = Grid::new(24, 80);

    // Write some text
    for c in "Hello, World!".chars() {
        grid.write_char(c);
    }
    grid.assert_invariants();

    // Move cursor
    grid.move_cursor_to(10, 40);
    grid.assert_invariants();

    // Scroll
    grid.scroll_up(5);
    grid.assert_invariants();

    // Resize
    grid.resize(30, 100);
    grid.assert_invariants();
}

#[test]
fn grid_cursor_bounds() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(100, 200);
    assert_eq!(grid.cursor_row(), 23);
    assert_eq!(grid.cursor_col(), 79);
}

#[test]
fn grid_cursor_movement() {
    let mut grid = Grid::new(24, 80);

    grid.move_cursor_to(10, 20);
    assert_eq!(grid.cursor(), Cursor::new(10, 20));

    grid.move_cursor_by(5, -10);
    assert_eq!(grid.cursor(), Cursor::new(15, 10));

    grid.move_cursor_by(-100, -100);
    assert_eq!(grid.cursor(), Cursor::new(0, 0));
}

#[test]
fn grid_cursor_up_within_scroll_region() {
    let mut grid = Grid::new(10, 80);
    // Set scroll region: rows 3-7
    grid.set_scroll_region(3, 7);
    // Cursor at row 5 (within region)
    grid.set_cursor(5, 10);
    // Move up 10 - should stop at top margin (row 3)
    grid.cursor_up(10);
    assert_eq!(grid.cursor_row(), 3);
}

#[test]
fn grid_cursor_up_outside_scroll_region() {
    let mut grid = Grid::new(10, 80);
    // Set scroll region: rows 3-7
    grid.set_scroll_region(3, 7);
    // Cursor at row 1 (above region)
    grid.set_cursor(1, 10);
    // Move up 10 - should stop at row 0
    grid.cursor_up(10);
    assert_eq!(grid.cursor_row(), 0);
}

#[test]
fn grid_cursor_down_within_scroll_region() {
    let mut grid = Grid::new(10, 80);
    // Set scroll region: rows 2-6
    grid.set_scroll_region(2, 6);
    // Cursor at row 4 (within region)
    grid.set_cursor(4, 10);
    // Move down 10 - should stop at bottom margin (row 6)
    grid.cursor_down(10);
    assert_eq!(grid.cursor_row(), 6);
}

#[test]
fn grid_cursor_down_outside_scroll_region() {
    let mut grid = Grid::new(10, 80);
    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);
    // Cursor at row 7 (below region)
    grid.set_cursor(7, 10);
    // Move down 10 - should stop at row 9 (last line)
    grid.cursor_down(10);
    assert_eq!(grid.cursor_row(), 9);
}

#[test]
fn grid_cursor_forward_stops_at_edge() {
    let mut grid = Grid::new(10, 80);
    grid.set_cursor(5, 70);
    grid.cursor_forward(20);
    assert_eq!(grid.cursor_col(), 79);
}

#[test]
fn grid_cursor_backward_stops_at_zero() {
    let mut grid = Grid::new(10, 80);
    grid.set_cursor(5, 10);
    grid.cursor_backward(20);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_cursor_movement_exact_amount() {
    let mut grid = Grid::new(10, 80);
    grid.set_cursor(5, 40);

    grid.cursor_up(3);
    assert_eq!(grid.cursor_row(), 2);

    grid.cursor_down(5);
    assert_eq!(grid.cursor_row(), 7);

    grid.cursor_forward(10);
    assert_eq!(grid.cursor_col(), 50);

    grid.cursor_backward(5);
    assert_eq!(grid.cursor_col(), 45);
}

#[test]
fn grid_write_char() {
    let mut grid = Grid::new(24, 80);
    grid.write_char('H');
    grid.write_char('i');

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'H');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'i');
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn grid_write_char_wrap() {
    let mut grid = Grid::new(24, 5);
    for c in "Hello World".chars() {
        grid.write_char_wrap(c);
    }

    // "Hello" on row 0, " Worl" on row 1, "d" on row 2
    assert_eq!(grid.row(0).unwrap().to_string(), "Hello");
    assert!(grid.row(1).unwrap().is_wrapped());
}

#[test]
fn write_char_wrap_marks_continuation_after_full_screen_scroll() {
    // REGRESSION: when the cursor autowraps at the bottom row of a FULL-screen
    // scroll region, the scroll recycles a fresh bottom row that receives the
    // soft-wrap continuation. That row must stay marked `wrapped`, or reflow /
    // selection / copy treats the continuation as a separate logical line and
    // inserts a spurious hard break.
    let mut grid = Grid::new(2, 2);
    for c in "ABCDE".chars() {
        grid.write_char_wrap(c);
    }
    // "AB" scrolled into scrollback; visible row 0 == "CD" (continuation of "AB"),
    // visible row 1 == "E" (continuation of "CD") — both are soft-wrap rows.
    assert_eq!(grid.row(0).unwrap().to_string(), "CD");
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'E');
    assert_eq!(grid.row_is_wrapped(0), Some(true));
    assert_eq!(
        grid.row_is_wrapped(1),
        Some(true),
        "the recycled bottom row holding the soft-wrap continuation must stay wrapped"
    );
}

#[test]
fn grid_line_feed() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(5, 10);
    grid.line_feed();
    assert_eq!(grid.cursor_row(), 6);
    assert_eq!(grid.cursor_col(), 10);
}

#[test]
fn grid_scroll_up() {
    let mut grid = Grid::new(3, 80);
    grid.write_char('A');
    grid.line_feed();
    grid.write_char('B');
    grid.line_feed();
    grid.write_char('C');

    // Now at bottom, scroll
    grid.line_feed();
    grid.write_char('D');

    // After scroll: row 0 has 'B' at col 1, row 1 has 'C' at col 2, row 2 has 'D' at col 3
    // (line_feed doesn't reset column, so each write is at increasing columns)
    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    assert_eq!(grid.cell(1, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 3).unwrap().char(), 'D');
}

#[test]
fn grid_resize() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(20, 70);
    grid.resize(10, 40);

    assert_eq!(grid.rows(), 10);
    assert_eq!(grid.cols(), 40);
    assert_eq!(grid.cursor_row(), 9);
    assert_eq!(grid.cursor_col(), 39);
}

#[test]
fn grid_save_restore_cursor() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(10, 20);
    grid.save_cursor();

    grid.set_cursor(0, 0);
    assert_eq!(grid.cursor(), Cursor::new(0, 0));

    grid.restore_cursor();
    assert_eq!(grid.cursor(), Cursor::new(10, 20));
}

#[test]
fn grid_erase_line() {
    let mut grid = Grid::new(24, 80);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    grid.erase_line();
    assert!(grid.row(0).unwrap().is_empty());
}

#[test]
fn grid_insert_chars() {
    let mut grid = Grid::new(24, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 3); // Position at 'D'
    grid.insert_chars(2);

    // Check that cells shifted: "ABC  DEFGH" (IJ pushed off)
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(0, 3).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 4).unwrap().char(), ' ');
    assert_eq!(grid.cell(0, 5).unwrap().char(), 'D');
}

#[test]
fn grid_delete_chars() {
    let mut grid = Grid::new(24, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 3); // Position at 'D'
    grid.delete_chars(2);

    // Check that cells shifted: "ABCFGHIJ  " (DE deleted)
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(0, 3).unwrap().char(), 'F');
    assert_eq!(grid.cell(0, 7).unwrap().char(), 'J');
    assert_eq!(grid.cell(0, 8).unwrap().char(), ' ');
}

#[test]
fn grid_insert_lines() {
    let mut grid = Grid::new(5, 10);
    // Write content to each row
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Row 0: A, Row 1: B, Row 2: C, Row 3: D, Row 4: E

    grid.set_cursor(1, 0); // At row 1
    grid.insert_lines(2);

    // Row 0: A (unchanged)
    // Row 1: (blank - inserted)
    // Row 2: (blank - inserted)
    // Row 3: B (shifted from row 1)
    // Row 4: C (shifted from row 2)
    // D and E pushed off

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'C');
}

#[test]
fn grid_delete_lines() {
    let mut grid = Grid::new(5, 10);
    // Write content to each row
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Row 0: A, Row 1: B, Row 2: C, Row 3: D, Row 4: E

    grid.set_cursor(1, 0); // At row 1
    grid.delete_lines(2);

    // Row 0: A (unchanged)
    // Row 1: D (shifted from row 3)
    // Row 2: E (shifted from row 4)
    // Row 3: (blank)
    // Row 4: (blank)

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'E');
    assert_eq!(grid.cell(3, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
}

#[test]
fn grid_screen_alignment_pattern() {
    let mut grid = Grid::new(3, 5);
    // Set some content first
    grid.write_char('X');
    grid.line_feed();
    grid.write_char('Y');

    // Set a scroll region
    grid.set_scroll_region(1, 2);

    grid.screen_alignment_pattern();

    // All cells should be 'E'
    for row in 0..3 {
        for col in 0..5 {
            assert_eq!(
                grid.cell(row, col).unwrap().char(),
                'E',
                "Cell ({row}, {col}) should be 'E'"
            );
        }
    }

    // Cursor should be at home
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);

    // Scroll region should be reset to full screen
    assert!(grid.scroll_region().is_full(3));
}

// =============================================================================
// row_text — complex characters and combining marks (#4517)
// =============================================================================

#[test]
fn test_row_text_complex_chars_and_combining_marks() {
    use std::sync::Arc;

    let mut grid = Grid::new(4, 20);

    // Write "Hi " at columns 0-2
    for c in "Hi ".chars() {
        grid.write_char(c);
    }

    // Set column 3 as a complex cell (emoji: 👋 U+1F44B, outside BMP).
    // Must set COMPLEX + has_extras flags so row_text reads from extras.
    let emoji = "👋";
    let row = 0u16;
    let col = 3u16;
    if let Some(r) = grid.row_mut(row) {
        let cell = r.get_mut(col).unwrap();
        cell.set_overflow_index(0);
        cell.set_has_extras(true);
        let mut flags = cell.flags();
        flags.insert(CellFlags::COMPLEX);
        cell.set_flags(flags);
    }
    let extra = grid.extras_mut().get_or_create(CellCoord::new(row, col));
    extra.set_complex_char(Some(Arc::from(emoji)));

    // Write 'e' at column 4 and add a combining acute accent (U+0301).
    // Must set has_extras so row_text reads combining marks (#7456).
    grid.move_cursor_to(0, 4);
    grid.write_char('e');
    if let Some(r) = grid.row_mut(row) {
        r.get_mut(4).unwrap().set_has_extras(true);
    }
    let extra = grid.extras_mut().get_or_create(CellCoord::new(row, 4));
    extra.add_combining('\u{0301}');

    let text = grid.row_text(row).unwrap();
    assert!(
        text.contains(emoji),
        "row_text should contain emoji '👋', got: {text:?}"
    );
    assert!(
        text.contains("e\u{0301}"),
        "row_text should contain 'e' + combining acute, got: {text:?}"
    );
    assert!(
        text.starts_with("Hi "),
        "row_text should start with 'Hi ', got: {text:?}"
    );
}

#[test]
fn test_row_text_bmp_only_unchanged() {
    let mut grid = Grid::new(4, 20);
    for c in "Hello World".chars() {
        grid.write_char(c);
    }
    let text = grid.row_text(0).unwrap();
    assert_eq!(text, "Hello World");
}

// --- Supplementary plane character tests (#5939) ---

#[test]
fn write_char_preserves_supplementary_plane_emoji() {
    let mut grid = Grid::new(4, 20);
    // U+1F600 (grinning face) is a supplementary plane character (> U+FFFF)
    grid.write_char('\u{1F600}');
    let text = grid.row_text(0).unwrap();
    assert!(
        text.contains('\u{1F600}'),
        "supplementary plane emoji should be preserved via overflow, got: {text:?}"
    );
}

#[test]
fn write_char_wrap_preserves_supplementary_plane_emoji() {
    let mut grid = Grid::new(4, 20);
    grid.write_char_wrap('\u{1F680}'); // rocket emoji U+1F680
    let text = grid.row_text(0).unwrap();
    assert!(
        text.contains('\u{1F680}'),
        "supplementary plane emoji via write_char_wrap should be preserved, got: {text:?}"
    );
}

#[test]
fn write_char_supplementary_sets_complex_flag() {
    let mut grid = Grid::new(4, 20);
    grid.write_char('\u{1F4A9}'); // pile of poo U+1F4A9
    let row = grid.row(0).unwrap();
    let cell = row.get(0).unwrap();
    assert!(
        cell.is_complex(),
        "non-BMP character cell should have COMPLEX flag set"
    );
}

#[test]
fn write_char_supplementary_mixed_with_ascii() {
    let mut grid = Grid::new(4, 40);
    // Write: "Hi " + rocket + " bye"
    for c in "Hi ".chars() {
        grid.write_char(c);
    }
    grid.write_char('\u{1F680}'); // rocket
    for c in " bye".chars() {
        grid.write_char(c);
    }
    let text = grid.row_text(0).unwrap();
    assert_eq!(text, "Hi \u{1F680} bye");
}

#[test]
fn write_char_bmp_not_affected_by_supplementary_fix() {
    let mut grid = Grid::new(4, 20);
    // CJK character (BMP, wide) — should still work directly
    grid.write_char('\u{4E16}'); // 世
    let text = grid.row_text(0).unwrap();
    assert!(
        text.contains('\u{4E16}'),
        "BMP character should still be stored directly, got: {text:?}"
    );
    let row = grid.row(0).unwrap();
    let cell = row.get(0).unwrap();
    assert!(
        !cell.is_complex(),
        "BMP character should NOT have COMPLEX flag"
    );
}
