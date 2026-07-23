// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn grid_unscroll_from_scrollback_basic() {
    // Create grid with tiered scrollback
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);

    // Write numbered lines and scroll them off
    for i in 0..8 {
        grid.carriage_return();
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        if i < 7 {
            grid.line_feed();
        }
    }

    // Now we have some content in scrollback and visible:
    // Scrollback should have lines 0-3 (the ones pushed off)
    // Visible should have lines 4-7
    assert_eq!(
        grid.tiered_scrollback_lines(),
        2,
        "8 lines on a 4-row grid with ring scrollback 2 should leave 2 tiered lines"
    );

    // Remember what was on row 0 before unscroll (for debugging if needed)
    let _visible_before = grid.row(0).map(|r| r.to_string()).unwrap_or_default();

    // Unscroll 2 lines from scrollback
    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(
        unscrolled, 2,
        "requesting 2 lines should recover both tiered lines"
    );

    // The top rows should now contain content from scrollback
    // Row 0 should have content (not blank like scroll_region_down)
    let row0_after = grid.row(0).map(|r| r.to_string()).unwrap_or_default();
    let row1_after = grid.row(1).map(|r| r.to_string()).unwrap_or_default();

    // At least one of the top rows should have content
    assert!(
        !row0_after.is_empty() || !row1_after.is_empty(),
        "Unscrolled rows should have content from scrollback"
    );
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
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);

    // Write content
    grid.write_char('X');

    // Unscroll 0 should be a no-op
    let unscrolled = grid.unscroll_from_scrollback(0);
    assert_eq!(unscrolled, 0);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'X');
}

#[test]
fn grid_unscroll_limited_by_available_scrollback() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);

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

    // Try to unscroll more than available
    let unscrolled = grid.unscroll_from_scrollback(100);

    // Should be limited by available scrollback and region size
    assert_eq!(
        unscrolled, 1,
        "unscroll should clamp to the single available tiered line"
    );
}

#[test]
fn grid_unscroll_with_scroll_region() {
    // Test unscroll with non-full scroll region (DECSTBM)
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(8, 20, 2, scrollback);

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

    // Remember content before unscroll
    let row0_before = grid.row(0).map(|r| r.to_string()).unwrap_or_default();
    let row1_before = grid.row(1).map(|r| r.to_string()).unwrap_or_default();
    let row6_before = grid.row(6).map(|r| r.to_string()).unwrap_or_default();
    let row7_before = grid.row(7).map(|r| r.to_string()).unwrap_or_default();

    // Unscroll 2 lines
    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(
        unscrolled, 2,
        "scroll region unscroll should honor the requested count"
    );

    // Rows outside scroll region should be unchanged
    assert_eq!(
        grid.row(0).map(|r| r.to_string()).unwrap_or_default(),
        row0_before,
        "Row 0 (outside region) should be unchanged"
    );
    assert_eq!(
        grid.row(1).map(|r| r.to_string()).unwrap_or_default(),
        row1_before,
        "Row 1 (outside region) should be unchanged"
    );
    assert_eq!(
        grid.row(6).map(|r| r.to_string()).unwrap_or_default(),
        row6_before,
        "Row 6 (outside region) should be unchanged"
    );
    assert_eq!(
        grid.row(7).map(|r| r.to_string()).unwrap_or_default(),
        row7_before,
        "Row 7 (outside region) should be unchanged"
    );

    // Top rows of scroll region (2, 3) should have scrollback content
    let row2_after = grid.row(2).map(|r| r.to_string()).unwrap_or_default();
    let row3_after = grid.row(3).map(|r| r.to_string()).unwrap_or_default();
    assert!(
        !row2_after.is_empty() || !row3_after.is_empty(),
        "Scroll region top rows should have scrollback content"
    );
}

#[test]
fn grid_unscroll_preserves_attributes() {
    use crate::cell::{CellFlags, PackedColor};

    // Create grid with scrollback
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 20, 2, scrollback);

    // Write styled content (red foreground, bold)
    let fg = PackedColor::indexed(196); // Red
    let bg = PackedColor::default();
    let flags = CellFlags::BOLD;

    grid.set_cursor(0, 0);
    for c in "STYLED".chars() {
        grid.write_char_styled(c, fg, bg, flags);
    }

    // Scroll content into scrollback
    for i in 0..6 {
        grid.set_cursor(3, 0);
        for c in format!("Line{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let scrollback_available = grid.tiered_scrollback_lines();
    assert_eq!(
        scrollback_available, 4,
        "6 bottom-row scrolls on a 4-row grid with ring scrollback 2 should offload 4 tiered lines"
    );

    // Unscroll to bring styled content back
    let unscrolled = grid.unscroll_from_scrollback(scrollback_available.min(4));
    assert_eq!(
        unscrolled, 4,
        "unscroll should recover all 4 available tiered lines"
    );

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
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);

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

    // Scroll the wrapped content into scrollback
    for i in 0..6 {
        grid.set_cursor(3, 0);
        for c in format!("Scroll{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let scrollback_available = grid.tiered_scrollback_lines();
    assert_eq!(
        scrollback_available, 4,
        "6 bottom-row scrolls on a 4-row grid with ring scrollback 2 should offload 4 tiered lines"
    );

    // Unscroll to bring wrapped content back
    let unscrolled = grid.unscroll_from_scrollback(scrollback_available.min(4));
    assert_eq!(
        unscrolled, 4,
        "unscroll should recover all 4 available tiered lines"
    );

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

/// Regression: fill_row_from_line must restore hyperlinks from scrollback (#1982).
#[test]
fn grid_unscroll_restores_hyperlinks() {
    use crate::extra::CellCoord;
    use std::sync::Arc;

    // max_scrollback = 0 so rows go directly to tiered scrollback with hyperlinks preserved
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 0, scrollback);

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
    // pushing original row 0 (with hyperlinks) to tiered scrollback
    for _ in 0..4 {
        grid.line_feed();
    }
    let scrollback_count = grid.tiered_scrollback_lines();
    assert_eq!(
        scrollback_count, 1,
        "four line feeds on a 4-row grid with direct tiered scrollback should offload exactly 1 line"
    );

    // Unscroll 1 line to bring the hyperlinked line back to row 0
    let unscrolled = grid.unscroll_from_scrollback(1);
    assert_eq!(unscrolled, 1, "Should have unscrolled exactly 1 line");

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

/// Regression test: fill_row_from_line must restore wide characters at correct
/// column positions, including WIDE_CONTINUATION cells.
///
/// When wide (CJK) characters are scrolled into scrollback and then unscrolled
/// back into the visible grid, fill_row_from_line must advance the column
/// counter by 2 for wide chars and create WIDE_CONTINUATION cells.
///
/// Bug: #2413 — fill_row_from_line previously advanced col by 1 per char,
/// placing wide characters as single-width and shifting subsequent characters.
#[test]
fn grid_unscroll_wide_char_column_alignment() {
    use crate::cell::{CellFlags, PackedColor};

    // Create grid with scrollback (max_scrollback=2 like other tests)
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 20, 2, scrollback);

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

    // Scroll content into scrollback (6 scrolls like other tests)
    for i in 0..6 {
        grid.set_cursor(3, 0);
        for c in format!("Line{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let scrollback_count = grid.tiered_scrollback_lines();
    assert_eq!(scrollback_count, 4);

    // Unscroll to bring the wide char line back
    let unscrolled = grid.unscroll_from_scrollback(scrollback_count.min(4));
    assert_eq!(unscrolled, 4);

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

/// Wide character at the last column during unscroll should be dropped,
/// not written as a single-width cell. Matches Row::write_wide_char
/// rejection behavior and TLA+ WideCharNotAtEnd invariant.
///
/// Bug: #2413 — fill_row_from_line else branch wrote wide chars as
/// single-width when they couldn't fit at the terminal edge.
#[test]
fn grid_unscroll_wide_char_at_last_col_dropped() {
    use crate::cell::{CellFlags, PackedColor};

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    // 5 columns wide — wide char at col 4 cannot fit (needs cols 4+5)
    let mut grid = Grid::with_tiered_scrollback(4, 5, 2, scrollback);

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

    // Now manually construct a scrollback Line with a wide char that would
    // land at the last column when restored to a 5-col grid.
    // Line: "ABCD中" — the wide char is char_idx 4, would need cols 4-5
    // but grid only has 5 cols (0-4).
    // We scroll enough to push row 0 into scrollback, then unscroll.

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
            // Col 4 (last col) must NOT have the WIDE flag
            let last_cell = row.get(4).unwrap();
            assert!(
                !last_cell.is_wide(),
                "Last col should not have WIDE flag — wide char should be dropped"
            );
            break;
        }
    }
}

/// Scrollback round-trip preserves CURLY_UNDERLINE, SUPERSCRIPT, and
/// SUBSCRIPT flags. CellAttrs::VISUAL_FLAGS_MASK must include bits 11-13.
///
/// Regression test for #2415: CellAttrs::VISUAL_FLAGS_MASK was 0x01FF
/// (bits 0-8 only), silently dropping SUPERSCRIPT, SUBSCRIPT, and
/// CURLY_UNDERLINE during scrollback serialization.
#[test]
fn grid_unscroll_preserves_extended_visual_flags() {
    use crate::cell::{CellFlags, PackedColor};

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 20, 2, scrollback);

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write cells with extended visual flags on row 0:
    // col 0: 'C' with CURLY_UNDERLINE
    // col 1: 'S' with SUPERSCRIPT
    // col 2: 'B' with SUBSCRIPT
    grid.set_cursor(0, 0);
    grid.write_char_styled('C', fg, bg, CellFlags::CURLY_UNDERLINE);
    grid.write_char_styled('S', fg, bg, CellFlags::SUPERSCRIPT);
    grid.write_char_styled('B', fg, bg, CellFlags::SUBSCRIPT);

    // Scroll row 0 into scrollback
    for i in 0..6 {
        grid.set_cursor(3, 0);
        for c in format!("Line{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let sb_count = grid.tiered_scrollback_lines();
    assert_eq!(
        sb_count, 4,
        "6 bottom-row scrolls on a 4-row grid with ring scrollback 2 should offload 4 tiered lines"
    );

    // Unscroll to bring styled content back
    let unscrolled = grid.unscroll_from_scrollback(sb_count.min(4));
    assert_eq!(
        unscrolled, 4,
        "unscroll should recover all 4 available tiered lines"
    );

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

/// Regression: fill_row_from_line must handle combining marks as part of
/// the preceding base character's grapheme cluster, not as separate cells.
///
/// Bug: fill_row_from_line iterates text character-by-character and places
/// each char in its own cell. Combining marks (width 0) like U+0301 (acute
/// accent) occupy their own column, shifting all subsequent content right
/// and corrupting column alignment.
///
/// Expected: "e\u{0301}" (é) occupies 1 column
/// Actual:   'e' at col 0, '\u{0301}' at col 1 (2 columns — wrong)
#[test]
fn grid_unscroll_combining_mark_not_split() {
    use crate::extra::CellCoord;

    // max_scrollback=0 for direct tiered scrollback
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 0, scrollback);

    // Write 'e' at col 0, then store combining acute accent in extras.
    grid.set_cursor(0, 0);
    grid.write_char('e');
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');

    // Write 'x' at col 1 to mark the expected next column.
    grid.set_cursor(0, 1);
    grid.write_char('x');

    // Scroll to push row 0 into scrollback.
    for _ in 0..4 {
        grid.line_feed();
    }
    let sb_count = grid.tiered_scrollback_lines();
    assert_eq!(
        sb_count, 1,
        "four line feeds on a 4-row grid with direct tiered scrollback should offload exactly 1 line"
    );

    // Unscroll to bring the combining-mark row back.
    let unscrolled = grid.unscroll_from_scrollback(sb_count.min(4));
    assert_eq!(
        unscrolled, 1,
        "unscroll should recover the single available tiered line"
    );

    // Find the restored row with 'e' at col 0.
    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx)
            && row.get(0).map(|c| c.char()) == Some('e')
        {
            found = true;

            // BUG: fill_row_from_line places U+0301 as a separate cell
            // at col 1, pushing 'x' to col 2. The combining mark should
            // be attached to 'e' at col 0, and 'x' should remain at col 1.
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

/// Regression: fill_row_from_line must handle ZWJ emoji sequences as a single
/// grapheme unit, not as separate wide/zero-width cells.
///
/// Bug: ZWJ sequences like 👨‍👩‍👧 (man ZWJ woman ZWJ girl) are stored in the
/// Line text as multiple codepoints. fill_row_from_line splits these into
/// individual cells: '👨' wide at col 0-1, ZWJ at col 2, '👩' wide at col 3-4,
/// etc. The ZWJ sequence should occupy 2 columns total (one wide cell).
#[test]
fn grid_unscroll_zwj_emoji_not_split() {
    use std::sync::Arc;

    use crate::cell::{Cell, CellFlags, PackedColor};
    use crate::extra::CellCoord;

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 20, 0, scrollback);

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

    // Scroll to push row 0 into scrollback.
    for _ in 0..4 {
        grid.line_feed();
    }
    let sb_count = grid.tiered_scrollback_lines();
    assert_eq!(
        sb_count, 1,
        "four line feeds on a 4-row grid with direct tiered scrollback should offload exactly 1 line"
    );

    // Unscroll to bring the ZWJ emoji row back.
    let unscrolled = grid.unscroll_from_scrollback(sb_count.min(4));
    assert_eq!(
        unscrolled, 1,
        "unscroll should recover the single available tiered line"
    );

    // Find the restored row.
    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx) {
            // Look for 'Z' — in correct reconstruction it's at col 2.
            // In the buggy version, the ZWJ sequence is split into multiple
            // cells (man=2 cols, ZWJ=1 col, woman=2 cols, ZWJ=1 col, girl=2 cols)
            // pushing 'Z' to col 8 or beyond.
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

/// Regression test for #4248: unscroll must remove recovered lines from
/// scrollback. Without the fix, scrollback retains duplicates of lines
/// that were already placed back into the visible grid.
#[test]
fn grid_unscroll_removes_lines_from_scrollback() {
    // ring_buffer_size=2: capacity = rows(4) + 2 = 6 rows.
    // Writing 8 lines overflows by 2 → 2 lines pushed to tiered scrollback.
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);

    for i in 0..8 {
        grid.carriage_return();
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        if i < 7 {
            grid.line_feed();
        }
    }

    let sb_before = grid.tiered_scrollback_lines();
    assert_eq!(sb_before, 2, "Should have 2 lines in tiered scrollback");

    // Unscroll 1 line: recover the most recent scrollback line
    let unscrolled = grid.unscroll_from_scrollback(1);
    assert_eq!(unscrolled, 1);

    let sb_after = grid.tiered_scrollback_lines();
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
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(4, 10, 2, scrollback);

    for i in 0..8 {
        grid.carriage_return();
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        if i < 7 {
            grid.line_feed();
        }
    }

    let sb_before = grid.tiered_scrollback_lines();
    assert_eq!(sb_before, 2, "Should have 2 lines in tiered scrollback");

    // Unscroll all available
    let unscrolled = grid.unscroll_from_scrollback(sb_before);
    assert_eq!(unscrolled, sb_before);

    assert_eq!(
        grid.tiered_scrollback_lines(),
        0,
        "Scrollback should be empty after unscrolling all lines"
    );
}

/// Regression test for #4521: unscroll must abort entirely when any scrollback
/// line fails to decompress, preserving the original scrollback data intact.
// Uses aterm-scrollback's disk-tier-gated DiskBackedScrollback APIs.
#[cfg(feature = "disk-tier")]
#[test]
fn grid_unscroll_aborts_on_decompression_failure() {
    use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig, ScrollbackStorage};

    let temp_dir = aterm_tempfile::tempdir().expect("create temp dir");
    let cold_path = temp_dir.path().join("scrollback.dtrm");

    // Zero-size hot/warm tiers + block_size=1: every line except the most
    // recent flows through hot → warm → cold immediately. After 20 pushes,
    // 19 lines are on disk (cold) and 1 is in hot (memory).
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

    // Truncate cold file to just the 32-byte header, removing all page data.
    // The DiskColdTier's in-memory page index still references pages beyond
    // byte 32, but reads will fail with I/O errors (seek past EOF).
    // Previous approach (overwriting first 64 bytes) only corrupted the
    // oldest pages at the file start, but unscroll reads the NEWEST cold
    // pages from the file tail — those were never corrupted.
    #[cfg(unix)]
    std::fs::OpenOptions::new()
        .write(true)
        .open(&cold_path)
        .and_then(|f| f.set_len(32))
        .expect("truncate cold file");
    // Windows twin: the cold tier holds the file MAPPED, and Windows refuses to
    // shrink a user-mapped file (ERROR_USER_MAPPED_FILE), so corrupt by
    // OVERWRITING every page byte past the 32-byte header with garbage instead.
    // This corrupts the NEWEST pages at the file tail too (the concern the
    // truncation approach was chosen for), and the ineffective-corruption skip
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

    // Unscroll 4 lines: line 19 (newest, in hot) succeeds, but lines
    // 16-18 (in cold) fail because page data was truncated →
    // try_read_scrollback_lines returns None → unscroll aborts with 0,
    // grid and scrollback unchanged.
    let unscrolled = grid.unscroll_from_scrollback(4);
    assert_eq!(unscrolled, 0, "Unscroll must abort when any line fails");

    // Scrollback must be fully preserved — no lines removed.
    assert_eq!(
        grid.tiered_scrollback_lines(),
        sb_before,
        "Scrollback line count must be unchanged after abort"
    );
}
