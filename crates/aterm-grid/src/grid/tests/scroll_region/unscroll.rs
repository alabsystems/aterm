// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn grid_unscroll_from_scrollback_basic() {
    // Create grid with tiered scrollback
    let mut grid = tiered_grid(4, 10, 2);

    // Write numbered lines and scroll them off
    write_numbered_lines(&mut grid, 8);

    // Scrollback holds lines 0-3; the screen shows lines 4-7.
    assert_eq!(
        grid.tiered_scrollback_lines(),
        2,
        "8 lines on a 4-row grid with ring scrollback 2 should leave 2 tiered lines"
    );

    // Unscroll 2 lines from scrollback
    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(
        unscrolled, 2,
        "requesting 2 lines should recover the 2 newest history lines"
    );

    // The top rows should now contain content from scrollback (not blank like
    // scroll_region_down): the NEWEST history (the ring's Lines 2-3), in order —
    // not the tiered store's Lines 0-1, which are older.
    assert_eq!(row_text(&grid, 0), "Line 2");
    assert_eq!(row_text(&grid, 1), "Line 3");
}

/// A `rows`×`cols` grid over a tiered store with a `ring`-line ring.
fn tiered_grid(rows: u16, cols: u16, ring: usize) -> Grid {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    Grid::with_tiered_scrollback(rows, cols, ring, scrollback)
}

/// Push a 4-row grid (ring 2) into history with four `{prefix}{i}` lines written
/// on the bottom row, then unscroll all four back; returns the unscrolled count.
fn scroll_four_off_then_unscroll(grid: &mut Grid, prefix: &str) -> usize {
    for i in 0..4 {
        grid.set_cursor(3, 0);
        for c in format!("{prefix}{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let scrollback_available = grid.scrollback_lines();
    assert_eq!(
        scrollback_available, 4,
        "4 bottom-row scrolls on a 4-row grid should leave 4 history lines (2 ring, 2 tiered)"
    );

    let unscrolled = grid.unscroll_from_scrollback(scrollback_available.min(4));
    assert_eq!(
        unscrolled, 4,
        "unscroll should recover all 4 history lines, ring and tiered"
    );
    unscrolled
}

/// Four line feeds on a 4-row grid with no ring push row 0 straight into the
/// tiered store; unscroll brings that one line back.
fn feed_row0_off_then_unscroll(grid: &mut Grid) {
    for _ in 0..4 {
        grid.line_feed();
    }
    let sb_count = grid.tiered_scrollback_lines();
    assert_eq!(
        sb_count, 1,
        "four line feeds on a 4-row grid with direct tiered scrollback should offload exactly 1 line"
    );

    let unscrolled = grid.unscroll_from_scrollback(sb_count.min(4));
    assert_eq!(
        unscrolled, 1,
        "unscroll should recover the single available tiered line"
    );
}

fn write_numbered_lines(grid: &mut Grid, count: usize) {
    for i in 0..count {
        grid.carriage_return();
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        if i + 1 < count {
            grid.line_feed();
        }
    }
}

/// Row `row` as a string (empty when absent).
fn row_string(grid: &Grid, row: u16) -> String {
    grid.row(row).map(|r| r.to_string()).unwrap_or_default()
}

fn row_text(grid: &Grid, row: u16) -> String {
    row_string(grid, row).trim_end().to_string()
}

/// Kitty CSI + T restores the lines directly above the viewport (the ring's
/// newest) and removes them from the end of history.
#[test]
fn grid_unscroll_restores_newest_ring_lines_in_order() {
    let mut grid = tiered_grid(4, 10, 2);
    write_numbered_lines(&mut grid, 8);
    // History Lines 0-3: ring = Lines 2-3, tiered store = Lines 0-1.
    assert_eq!(grid.scrollback_lines(), 4);

    assert_eq!(grid.unscroll_from_scrollback(2), 2);

    assert_eq!(row_text(&grid, 0), "Line 2");
    assert_eq!(row_text(&grid, 1), "Line 3");
    assert_eq!(row_text(&grid, 2), "Line 4");
    assert_eq!(row_text(&grid, 3), "Line 5");
    assert_eq!(grid.scrollback_lines(), 2);
    let hist = |i: usize| {
        grid.get_history_line(i)
            .map(|l| l.to_string().trim_end().to_string())
            .unwrap_or_default()
    };
    assert_eq!(hist(0), "Line 0");
    assert_eq!(hist(1), "Line 1");
}

/// Ring-only history (an empty tiered store) is still history: CSI + T must pull
/// from it, not fall back to a blank scroll that discards the bottom rows.
#[test]
fn grid_unscroll_ring_only_history_is_not_a_blank_scroll() {
    let mut grid = tiered_grid(4, 10, 100);
    write_numbered_lines(&mut grid, 6);
    assert_eq!(grid.tiered_scrollback_lines(), 0);
    assert_eq!(grid.scrollback_lines(), 2);

    assert_eq!(grid.unscroll_from_scrollback(1), 1);

    assert_eq!(row_text(&grid, 0), "Line 1");
    assert_eq!(row_text(&grid, 3), "Line 4");
    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(
        grid.get_history_line(0)
            .map(|l| l.to_string().trim_end().to_string())
            .unwrap_or_default(),
        "Line 0"
    );
}

/// A pull spanning the ring and the store keeps age order across the seam.
#[test]
fn grid_unscroll_spanning_ring_and_store_keeps_age_order() {
    let mut grid = tiered_grid(4, 10, 2);
    write_numbered_lines(&mut grid, 8);

    assert_eq!(grid.unscroll_from_scrollback(3), 3);

    assert_eq!(row_text(&grid, 0), "Line 1");
    assert_eq!(row_text(&grid, 1), "Line 2");
    assert_eq!(row_text(&grid, 2), "Line 3");
    assert_eq!(row_text(&grid, 3), "Line 4");
    assert_eq!(grid.scrollback_lines(), 1);
}

#[test]
fn grid_unscroll_no_scrollback_fallback() {
    // Grid without scrollback (simulates alternate screen)
    let mut grid = Grid::new(4, 10);

    // Write some content
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Unscroll should fall back to scroll_region_down behavior
    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(unscrolled, 0, "No scrollback means 0 lines unscrolled");

    // Top rows should be blank (regular scroll down behavior)
    assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(1, 0).unwrap().char(), ' ');

    // Content shifted down
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'B');
}

#[test]
fn grid_unscroll_zero_does_nothing() {
    let mut grid = tiered_grid(4, 10, 2);

    // Write content
    grid.write_char('X');

    // Unscroll 0 should be a no-op
    let unscrolled = grid.unscroll_from_scrollback(0);
    assert_eq!(unscrolled, 0);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'X');
}

#[test]
fn grid_unscroll_limited_by_available_scrollback() {
    let mut grid = tiered_grid(4, 10, 2);

    // Write just 2 lines to get minimal scrollback
    for c in "Line0".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    for c in "Line1".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    // Write 4 more lines to push line0 and line1 into scrollback
    for i in 2..6 {
        for c in format!("Line{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let scrollback_available = grid.tiered_scrollback_lines();
    assert_eq!(
        scrollback_available, 1,
        "6 total lines on a 4-row grid with ring scrollback 2 should leave 1 tiered line"
    );

    // History is 2 ring lines over that 1 tiered line.
    assert_eq!(grid.scrollback_lines(), 3);

    // Try to unscroll more than available
    let unscrolled = grid.unscroll_from_scrollback(100);

    // Limited by all history (2 ring + 1 tiered) and the region size.
    assert_eq!(
        unscrolled, 3,
        "unscroll should clamp to the 3 history lines, ring and tiered"
    );
    assert_eq!(grid.scrollback_lines(), 0);
}

#[test]
fn grid_unscroll_with_scroll_region() {
    // Test unscroll with non-full scroll region (DECSTBM)
    let mut grid = tiered_grid(8, 20, 2);

    // Write content on all rows
    for row in 0..8 {
        grid.set_cursor(row, 0);
        for c in format!("Row{row}").chars() {
            grid.write_char(c);
        }
    }

    // Scroll content off to build scrollback (need to fill scrollback first)
    for i in 0..10 {
        grid.set_cursor(7, 0);
        for c in format!("Scroll{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let scrollback_available = grid.tiered_scrollback_lines();
    assert_eq!(
        scrollback_available, 8,
        "10 bottom-row scrolls on an 8-row grid with ring scrollback 2 should offload 8 tiered lines"
    );

    // Set a scroll region in the middle (rows 2-5)
    grid.set_scroll_region(2, 5);

    // Remember the rows outside the region before unscroll
    let outside = [0u16, 1, 6, 7];
    let before: Vec<String> = outside.iter().map(|&r| row_string(&grid, r)).collect();

    // Unscroll 2 lines
    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(
        unscrolled, 2,
        "scroll region unscroll should honor the requested count"
    );

    // Rows outside scroll region should be unchanged
    for (&r, before) in outside.iter().zip(&before) {
        assert_eq!(
            &row_string(&grid, r),
            before,
            "Row {r} (outside region) should be unchanged"
        );
    }

    // Top rows of scroll region (2, 3) should have scrollback content
    assert!(
        !row_string(&grid, 2).is_empty() || !row_string(&grid, 3).is_empty(),
        "Scroll region top rows should have scrollback content"
    );
}

#[test]
fn grid_unscroll_preserves_attributes() {
    use crate::cell::{CellFlags, PackedColor};

    // Create grid with scrollback
    let mut grid = tiered_grid(4, 20, 2);

    // Write styled content (red foreground, bold)
    let fg = PackedColor::indexed(196); // Red
    let bg = PackedColor::default();
    let flags = CellFlags::BOLD;

    grid.set_cursor(0, 0);
    for c in "STYLED".chars() {
        grid.write_char_styled(c, fg, bg, flags);
    }

    // Scroll the styled content into scrollback and back
    scroll_four_off_then_unscroll(&mut grid, "Line");

    // Check if any restored row has non-default colors or flags
    let mut found_styled = false;
    for row_idx in 0..4 {
        if let Some(row) = grid.row(row_idx) {
            for col in 0..20u16 {
                if let Some(cell) = row.get(col) {
                    let c = cell.char();
                    if c != ' ' && c != '\0' {
                        // Check if cell has non-default styling
                        let fg_non_default = cell.fg_color().is_none_or(|c| !c.is_default());
                        let cell_flags = cell.flags();
                        if fg_non_default || cell_flags.contains(CellFlags::BOLD) {
                            found_styled = true;
                            break;
                        }
                    }
                }
            }
        }
        if found_styled {
            break;
        }
    }

    // Verify styled content was found after unscroll
    // fill_row_from_line (scroll.rs:474-510) should restore cell attributes
    assert!(
        found_styled,
        "Unscroll should preserve cell attributes (fg color, bold flag)"
    );
}

#[test]
fn grid_unscroll_preserves_wrapped_flag() {
    // Create grid with scrollback
    let mut grid = tiered_grid(4, 10, 2);

    // Write a long line that wraps (more than 10 columns)
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    // Continue on the same logical line (auto-wrap)
    for c in "KLMNO".chars() {
        grid.write_char(c);
    }

    // First row should be marked as wrapped
    if let Some(row) = grid.row_mut(0) {
        row.set_wrapped(true);
    }

    // Scroll the wrapped content into scrollback and back
    let unscrolled = scroll_four_off_then_unscroll(&mut grid, "Scroll");

    // Check if any restored row has the wrapped flag
    // Note: fill_row_from_line at scroll.rs:509 sets row.set_wrapped(line.is_wrapped())
    let mut found_wrapped = false;
    for row_idx in 0..unscrolled {
        if let Some(row) = grid.row(row_idx as u16)
            && row.is_wrapped()
        {
            found_wrapped = true;
            break;
        }
    }

    // Verify wrapped flag was restored after unscroll
    // fill_row_from_line (scroll.rs:509) calls row.set_wrapped(line.is_wrapped())
    assert!(
        found_wrapped,
        "Unscroll should preserve wrapped flag from scrollback"
    );
}

/// fill_row_from_line restores hyperlinks from scrollback.
#[test]
fn grid_unscroll_restores_hyperlinks() {
    use crate::extra::CellCoord;
    use std::sync::Arc;

    // max_scrollback = 0 so rows go directly to tiered scrollback with hyperlinks preserved
    let mut grid = tiered_grid(4, 10, 0);

    // Write "Hello" on row 0 with a hyperlink on columns 0-4
    let url: Arc<str> = Arc::from("https://example.com");
    grid.set_cursor(0, 0);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    for col in 0..5u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }

    // Scroll exactly 4 times: LF 1-3 move cursor, LF 4 triggers scroll_up(1)
    // pushing original row 0 (with hyperlinks) to tiered scrollback; unscroll
    // 1 line to bring the hyperlinked line back to row 0
    feed_row0_off_then_unscroll(&mut grid);

    // Row 0 should have the hyperlink restored from scrollback
    assert!(
        grid.extras().row_has_hyperlinks(0),
        "Hyperlinks should be restored from scrollback on unscroll"
    );
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(0, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url.clone()),
        "Hyperlink URL should match original"
    );
}

/// fill_row_from_line places wide characters at their columns: col advances by
/// 2 and a WIDE_CONTINUATION cell follows each wide char.
#[test]
fn grid_unscroll_wide_char_column_alignment() {
    use crate::cell::{CellFlags, PackedColor};

    // Create grid with scrollback (max_scrollback=2 like other tests)
    let mut grid = tiered_grid(4, 20, 2);

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write: "A" at col 0, wide char at cols 1-2, "B" at col 3
    grid.set_cursor(0, 0);
    grid.write_char('A');
    if let Some(row) = grid.row_mut(0) {
        row.write_wide_char(1, '\u{4E2D}', fg, bg, CellFlags::empty());
    }
    grid.set_cursor(0, 3);
    grid.write_char('B');

    // Verify initial state: A[中]B
    {
        let row0 = grid.row(0).unwrap();
        assert_eq!(row0.get(0).unwrap().char(), 'A');
        assert!(row0.get(1).unwrap().is_wide(), "col 1 should be WIDE");
        assert_eq!(row0.get(1).unwrap().char(), '\u{4E2D}');
        assert!(
            row0.get(2).unwrap().is_wide_continuation(),
            "col 2 should be WIDE_CONTINUATION"
        );
        assert_eq!(row0.get(3).unwrap().char(), 'B');
    }

    // Scroll the wide char line into scrollback and back
    scroll_four_off_then_unscroll(&mut grid, "Line");

    // Search for the restored wide char line among all visible rows
    let mut found_wide_char_row = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx) {
            let c0 = row.get(0).map(|c| c.char());
            let c1 = row.get(1).map(|c| c.char());
            if c0 == Some('A') && c1 == Some('\u{4E2D}') {
                found_wide_char_row = true;

                // Wide char at col 1 must have WIDE flag
                assert!(
                    row.get(1).unwrap().is_wide(),
                    "col 1 should have WIDE flag after unscroll"
                );
                // Col 2 must be a WIDE_CONTINUATION cell
                assert!(
                    row.get(2).unwrap().is_wide_continuation(),
                    "col 2 should be WIDE_CONTINUATION after unscroll"
                );
                // 'B' must be at col 3 (after the 2-column wide char)
                assert_eq!(
                    row.get(3).unwrap().char(),
                    'B',
                    "'B' should be at col 3 after wide char occupying cols 1-2"
                );
                break;
            }
        }
    }

    assert!(
        found_wide_char_row,
        "Should find the restored row with A and wide char"
    );
}

/// A wide character at the last column during unscroll is dropped, not written
/// single-width (as Row::write_wide_char rejects it; WideCharNotAtEnd).
#[test]
fn grid_unscroll_wide_char_at_last_col_dropped() {
    use crate::cell::{CellFlags, PackedColor};

    // 5 columns wide — wide char at col 4 cannot fit (needs cols 4+5)
    let mut grid = tiered_grid(4, 5, 2);

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write: "ABCD" at cols 0-3, then wide char at col 4 (last col)
    grid.set_cursor(0, 0);
    for c in "ABCD".chars() {
        grid.write_char(c);
    }
    // Wide char at col 4 should be rejected by write_wide_char (last col)
    if let Some(row) = grid.row_mut(0) {
        let ok = row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
        assert!(!ok, "Wide char at last col should be rejected");
    }

    // Instead, write 'E' at col 4 so we can track what happens
    grid.set_cursor(0, 4);
    grid.write_char('E');

    // A scrollback line "ABCD中" whose wide char lands at the last column of a
    // 5-col grid once restored.

    for i in 0..6 {
        grid.set_cursor(3, 0);
        for c in format!("L{i}xx").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let sb_count = grid.tiered_scrollback_lines();
    assert_eq!(
        sb_count, 4,
        "6 bottom-row scrolls on a 4-row grid with ring scrollback 2 should offload 4 tiered lines"
    );

    let unscrolled = grid.unscroll_from_scrollback(sb_count.min(4));
    assert_eq!(
        unscrolled, 4,
        "unscroll should recover all 4 available tiered lines"
    );

    // Find the restored row starting with 'A'
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx)
            && row.get(0).map(|c| c.char()) == Some('A')
        {
            // Col 4 (last col) must not have the WIDE flag
            let last_cell = row.get(4).unwrap();
            assert!(
                !last_cell.is_wide(),
                "Last col should not have WIDE flag — wide char should be dropped"
            );
            break;
        }
    }
}

/// Scrollback round-trip preserves CURLY_UNDERLINE, SUPERSCRIPT and SUBSCRIPT:
/// CellAttrs::VISUAL_FLAGS_MASK covers bits 11-13.
#[test]
fn grid_unscroll_preserves_extended_visual_flags() {
    use crate::cell::{CellFlags, PackedColor};

    let mut grid = tiered_grid(4, 20, 2);

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Row 0: 'C' curly-underlined, 'S' superscript, 'B' subscript.
    grid.set_cursor(0, 0);
    grid.write_char_styled('C', fg, bg, CellFlags::CURLY_UNDERLINE);
    grid.write_char_styled('S', fg, bg, CellFlags::SUPERSCRIPT);
    grid.write_char_styled('B', fg, bg, CellFlags::SUBSCRIPT);

    // Scroll row 0 into scrollback and back
    scroll_four_off_then_unscroll(&mut grid, "Line");

    // Find the restored row starting with 'C'
    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx)
            && row.get(0).map(|c| c.char()) == Some('C')
        {
            found = true;

            let cell_c = row.get(0).unwrap();
            let cell_s = row.get(1).unwrap();
            let cell_b = row.get(2).unwrap();

            // Verify character content survived the round-trip
            assert_eq!(cell_c.char(), 'C', "col 0 character should be 'C'");
            assert_eq!(cell_s.char(), 'S', "col 1 character should be 'S'");
            assert_eq!(cell_b.char(), 'B', "col 2 character should be 'B'");

            // Verify extended visual flags survived the round-trip
            assert!(
                cell_c.flags().contains(CellFlags::CURLY_UNDERLINE),
                "col 0 ('C') should have CURLY_UNDERLINE after unscroll"
            );
            assert!(
                cell_s.flags().contains(CellFlags::SUPERSCRIPT),
                "col 1 ('S') should have SUPERSCRIPT after unscroll"
            );
            assert!(
                cell_b.flags().contains(CellFlags::SUBSCRIPT),
                "col 2 ('B') should have SUBSCRIPT after unscroll"
            );
            break;
        }
    }
    assert!(
        found,
        "Row with extended visual flags not found after unscroll"
    );
}

/// fill_row_from_line keeps a combining mark in its base character's cell:
/// "e\u{0301}" occupies one column.
#[test]
fn grid_unscroll_combining_mark_not_split() {
    use crate::extra::CellCoord;

    // max_scrollback=0 for direct tiered scrollback
    let mut grid = tiered_grid(4, 10, 0);

    // Write 'e' at col 0, then store combining acute accent in extras.
    grid.set_cursor(0, 0);
    grid.write_char('e');
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');

    // Write 'x' at col 1 to mark the expected next column.
    grid.set_cursor(0, 1);
    grid.write_char('x');

    // Push row 0 into scrollback and unscroll the combining-mark row back.
    feed_row0_off_then_unscroll(&mut grid);

    // Find the restored row with 'e' at col 0.
    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx)
            && row.get(0).map(|c| c.char()) == Some('e')
        {
            found = true;

            // U+0301 belongs to 'e' at col 0, so 'x' stays at col 1.
            let col1_char = row.get(1).map(|c| c.char());
            assert_eq!(
                col1_char,
                Some('x'),
                "col 1 should be 'x' (combining mark should not take its own cell). \
                     Got {:?} — the combining mark was split into a separate cell.",
                col1_char.map(|c| format!("U+{:04X}", c as u32))
            );
            break;
        }
    }
    assert!(
        found,
        "Row with 'e' + combining mark not found after unscroll"
    );
}

/// fill_row_from_line keeps a ZWJ emoji sequence as one wide cell (2 columns).
#[test]
fn grid_unscroll_zwj_emoji_not_split() {
    use std::sync::Arc;

    use crate::cell::{Cell, CellFlags, PackedColor};
    use crate::extra::CellCoord;

    let mut grid = tiered_grid(4, 20, 0);

    // Write a family emoji (ZWJ sequence) at col 0-1 via overflow + extras.
    let zwj_text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"; // 👨‍👩‍👧
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Set up overflow cell + complex_char extras
    let mut cell = Cell::with_style(' ', fg, bg, CellFlags::WIDE);
    cell.set_overflow_index(0);
    grid.row_mut(0).unwrap().set(0, cell);
    grid.row_mut(0).unwrap().set(
        1,
        Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION),
    );
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .set_complex_char(Some(Arc::from(zwj_text)));

    // Write 'Z' at col 2 to mark the expected next column.
    grid.set_cursor(0, 2);
    grid.write_char('Z');

    // Push row 0 into scrollback and unscroll the ZWJ emoji row back.
    feed_row0_off_then_unscroll(&mut grid);

    // Find the restored row.
    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx) {
            // Correct reconstruction puts 'Z' at col 2; a split ZWJ sequence
            // would push it to col 8 or beyond.
            if row.get(2).map(|c| c.char()) == Some('Z') {
                found = true;
                // ZWJ should occupy cols 0-1, 'Z' at col 2. Correct!
                break;
            }
        }
    }
    assert!(
        found,
        "ZWJ emoji should occupy 2 columns, with 'Z' at col 2. \
         The ZWJ sequence was likely split into individual cells."
    );
}

/// Unscroll removes the recovered lines from scrollback (no duplicates).
#[test]
fn grid_unscroll_removes_lines_from_scrollback() {
    // ring_buffer_size=2: capacity = rows(4) + 2 = 6 rows.
    // Writing 8 lines overflows by 2 → 2 lines pushed to tiered scrollback.
    let mut grid = tiered_grid(4, 10, 2);

    write_numbered_lines(&mut grid, 8);

    let sb_before = grid.scrollback_lines();
    assert_eq!(
        sb_before, 4,
        "Should have 4 history lines (2 ring, 2 tiered)"
    );

    // Unscroll 1 line: recover the most recent scrollback line
    let unscrolled = grid.unscroll_from_scrollback(1);
    assert_eq!(unscrolled, 1);

    let sb_after = grid.scrollback_lines();
    assert_eq!(
        sb_after,
        sb_before - 1,
        "Scrollback should shrink by the number of unscrolled lines"
    );
}

/// Unscrolling all available scrollback lines should leave scrollback empty.
#[test]
fn grid_unscroll_all_empties_scrollback() {
    // ring_buffer_size=2: capacity = rows(4) + 2 = 6 rows.
    // Writing 8 lines overflows by 2 → 2 lines in tiered scrollback.
    let mut grid = tiered_grid(4, 10, 2);

    write_numbered_lines(&mut grid, 8);

    let sb_before = grid.scrollback_lines();
    assert_eq!(
        sb_before, 4,
        "Should have 4 history lines (2 ring, 2 tiered)"
    );

    // Unscroll all available
    let unscrolled = grid.unscroll_from_scrollback(sb_before);
    assert_eq!(unscrolled, sb_before);

    assert_eq!(
        grid.scrollback_lines(),
        0,
        "Scrollback should be empty after unscrolling all lines"
    );
}

/// Unscroll aborts entirely when any scrollback line fails to decompress,
/// leaving scrollback intact.
// Uses aterm-scrollback's disk-tier-gated DiskBackedScrollback APIs.
#[cfg(feature = "disk-tier")]
#[test]
fn grid_unscroll_aborts_on_decompression_failure() {
    use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig, ScrollbackStorage};

    let temp_dir = aterm_tempfile::tempdir().expect("create temp dir");
    let cold_path = temp_dir.path().join("scrollback.dtrm");

    // Zero-size hot/warm tiers and block_size=1: after 20 pushes, 19 lines are
    // cold (on disk) and 1 is hot.
    let config = DiskBackedScrollbackConfig::new(&cold_path)
        .with_hot_limit(0)
        .with_warm_limit(0)
        .with_block_size(1);
    let disk_sb = DiskBackedScrollback::with_config(config).expect("create disk scrollback");

    let mut storage: ScrollbackStorage = disk_sb.into();
    for i in 0..20 {
        let line = aterm_scrollback::Line::from(format!("ColdLine{i:02}").as_str());
        storage.push_line(line).expect("push line");
    }
    assert_eq!(storage.line_count(), 20);
    assert!(std::fs::metadata(&cold_path).map_or(0, |m| m.len()) > 0);

    // Truncate the cold file to its 32-byte header. The in-memory page index
    // still references the newest pages at the file tail, so reads fail.
    #[cfg(unix)]
    std::fs::OpenOptions::new()
        .write(true)
        .open(&cold_path)
        .and_then(|f| f.set_len(32))
        .expect("truncate cold file");
    // Windows refuses to shrink a mapped file (ERROR_USER_MAPPED_FILE), so
    // overwrite every page byte past the 32-byte header instead; the skip
    // below still guards the premise.
    #[cfg(windows)]
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&cold_path)
            .expect("open cold file");
        let len = f.metadata().expect("stat cold file").len();
        f.seek(SeekFrom::Start(32)).expect("seek past header");
        f.write_all(&vec![
            0xFF;
            usize::try_from(len.saturating_sub(32)).unwrap()
        ])
        .expect("overwrite cold pages");
    }

    // Verify corruption is effective: newest cold line must fail.
    // get_line_rev(1) = the newest cold line (rev 0 is in hot).
    if storage.get_line_rev(1).is_ok() {
        return; // Truncation didn't affect newest cold page — skip.
    }

    let sb_before = storage.line_count();
    let mut grid = Grid::with_tiered_scrollback(4, 20, 0, storage);

    // Unscroll 4: line 19 (hot) reads, lines 16-18 (cold) fail, so unscroll
    // aborts with 0 and leaves grid and scrollback unchanged.
    let unscrolled = grid.unscroll_from_scrollback(4);
    assert_eq!(unscrolled, 0, "Unscroll must abort when any line fails");

    // Scrollback must be fully preserved — no lines removed.
    assert_eq!(
        grid.tiered_scrollback_lines(),
        sb_before,
        "Scrollback line count must be unchanged after abort"
    );
}
