// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

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
    // No scrollback (the alternate screen's shape): unscroll falls back to a blank scroll down.
    let mut grid = Grid::new(4, 10);

    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(unscrolled, 0, "No scrollback means 0 lines unscrolled");

    // Top rows are blank, content shifted down.
    assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(1, 0).unwrap().char(), ' ');

    assert_eq!(grid.cell(2, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'B');
}

#[test]
fn grid_unscroll_limited_by_available_scrollback() {
    let mut grid = tiered_grid(4, 10, 2);

    for c in "Line0".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    for c in "Line1".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
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

    assert_eq!(grid.scrollback_lines(), 3);

    let unscrolled = grid.unscroll_from_scrollback(100);

    assert_eq!(
        unscrolled, 3,
        "unscroll should clamp to the 3 history lines, ring and tiered"
    );
    assert_eq!(grid.scrollback_lines(), 0);
}

#[test]
fn grid_unscroll_with_scroll_region() {
    // Unscroll inside a partial scroll region (DECSTBM).
    let mut grid = tiered_grid(8, 20, 2);

    for row in 0..8 {
        grid.set_cursor(row, 0);
        for c in format!("Row{row}").chars() {
            grid.write_char(c);
        }
    }

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

    grid.set_scroll_region(2, 5);

    let outside = [0u16, 1, 6, 7];
    let before: Vec<String> = outside.iter().map(|&r| row_string(&grid, r)).collect();

    let unscrolled = grid.unscroll_from_scrollback(2);
    assert_eq!(
        unscrolled, 2,
        "scroll region unscroll should honor the requested count"
    );

    for (&r, before) in outside.iter().zip(&before) {
        assert_eq!(
            &row_string(&grid, r),
            before,
            "Row {r} (outside region) should be unchanged"
        );
    }

    assert!(
        !row_string(&grid, 2).is_empty() || !row_string(&grid, 3).is_empty(),
        "Scroll region top rows should have scrollback content"
    );
}

#[test]
fn grid_unscroll_preserves_attributes() {
    use crate::cell::{CellFlags, PackedColor};

    let mut grid = tiered_grid(4, 20, 2);

    let fg = PackedColor::indexed(196); // Red
    let bg = PackedColor::default();
    let flags = CellFlags::BOLD;

    grid.set_cursor(0, 0);
    for c in "STYLED".chars() {
        grid.write_char_styled(c, fg, bg, flags);
    }

    scroll_four_off_then_unscroll(&mut grid, "Line");

    let mut found_styled = false;
    for row_idx in 0..4 {
        if let Some(row) = grid.row(row_idx) {
            for col in 0..20u16 {
                if let Some(cell) = row.get(col) {
                    let c = cell.char();
                    if c != ' ' && c != '\0' {
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

    // `fill_row_from_line` (grid/scroll_fill.rs) restores cell attributes.
    assert!(
        found_styled,
        "Unscroll should preserve cell attributes (fg color, bold flag)"
    );
}

#[test]
fn grid_unscroll_preserves_wrapped_flag() {
    let mut grid = tiered_grid(4, 10, 2);

    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    for c in "KLMNO".chars() {
        grid.write_char(c);
    }

    if let Some(row) = grid.row_mut(0) {
        row.set_wrapped(true);
    }

    let unscrolled = scroll_four_off_then_unscroll(&mut grid, "Scroll");

    // `fill_row_from_line` (grid/scroll_fill.rs) restores the line's wrapped flag.
    let mut found_wrapped = false;
    for row_idx in 0..unscrolled {
        if let Some(row) = grid.row(row_idx as u16)
            && row.is_wrapped()
        {
            found_wrapped = true;
            break;
        }
    }

    assert!(
        found_wrapped,
        "Unscroll should preserve wrapped flag from scrollback"
    );
}

/// `fill_row_from_line` restores hyperlinks from scrollback.
#[test]
fn grid_unscroll_restores_hyperlinks() {
    use crate::extra::CellCoord;
    use std::sync::Arc;

    // No ring: rows go straight to the tiered store with their hyperlinks.
    let mut grid = tiered_grid(4, 10, 0);

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

    // The fourth LF pushes row 0, links and all, into the tiered store; unscroll brings it back.
    feed_row0_off_then_unscroll(&mut grid);

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

/// `fill_row_from_line` places wide chars at their columns: each is followed by a
/// WIDE_CONTINUATION cell.
#[test]
fn grid_unscroll_wide_char_column_alignment() {
    use crate::cell::{CellFlags, PackedColor};

    let mut grid = tiered_grid(4, 20, 2);

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // A[中]B: 'A' at col 0, the wide char at cols 1-2, 'B' at col 3.
    grid.set_cursor(0, 0);
    grid.write_char('A');
    if let Some(row) = grid.row_mut(0) {
        row.write_wide_char(1, '\u{4E2D}', fg, bg, CellFlags::empty());
    }
    grid.set_cursor(0, 3);
    grid.write_char('B');

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

    scroll_four_off_then_unscroll(&mut grid, "Line");

    let mut found_wide_char_row = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx) {
            let c0 = row.get(0).map(|c| c.char());
            let c1 = row.get(1).map(|c| c.char());
            if c0 == Some('A') && c1 == Some('\u{4E2D}') {
                found_wide_char_row = true;

                assert!(
                    row.get(1).unwrap().is_wide(),
                    "col 1 should have WIDE flag after unscroll"
                );
                assert!(
                    row.get(2).unwrap().is_wide_continuation(),
                    "col 2 should be WIDE_CONTINUATION after unscroll"
                );
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

/// A scrollback round trip keeps CURLY_UNDERLINE, SUPERSCRIPT and SUBSCRIPT (bits 11-13, inside
/// `CellFlags::VISUAL_FLAGS_MASK`).
#[test]
fn grid_unscroll_preserves_extended_visual_flags() {
    use crate::cell::{CellFlags, PackedColor};

    let mut grid = tiered_grid(4, 20, 2);

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    grid.set_cursor(0, 0);
    grid.write_char_styled('C', fg, bg, CellFlags::CURLY_UNDERLINE);
    grid.write_char_styled('S', fg, bg, CellFlags::SUPERSCRIPT);
    grid.write_char_styled('B', fg, bg, CellFlags::SUBSCRIPT);

    scroll_four_off_then_unscroll(&mut grid, "Line");

    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx)
            && row.get(0).map(|c| c.char()) == Some('C')
        {
            found = true;

            let cell_c = row.get(0).unwrap();
            let cell_s = row.get(1).unwrap();
            let cell_b = row.get(2).unwrap();

            assert_eq!(cell_c.char(), 'C', "col 0 character should be 'C'");
            assert_eq!(cell_s.char(), 'S', "col 1 character should be 'S'");
            assert_eq!(cell_b.char(), 'B', "col 2 character should be 'B'");

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

    // No ring: rows go straight to the tiered store.
    let mut grid = tiered_grid(4, 10, 0);

    grid.set_cursor(0, 0);
    grid.write_char('e');
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');

    grid.set_cursor(0, 1);
    grid.write_char('x');

    feed_row0_off_then_unscroll(&mut grid);

    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx)
            && row.get(0).map(|c| c.char()) == Some('e')
        {
            found = true;

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

    // A family emoji (ZWJ sequence) at cols 0-1 via an overflow cell and extras.
    let zwj_text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"; // 👨‍👩‍👧
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

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

    grid.set_cursor(0, 2);
    grid.write_char('Z');

    feed_row0_off_then_unscroll(&mut grid);

    let mut found = false;
    for row_idx in 0..4u16 {
        if let Some(row) = grid.row(row_idx) {
            // A split ZWJ sequence would push 'Z' to col 8 or beyond.
            if row.get(2).map(|c| c.char()) == Some('Z') {
                found = true;
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

/// Unscroll aborts entirely when any scrollback line fails to decompress,
/// leaving scrollback intact.
#[test]
fn grid_unscroll_aborts_on_decompression_failure() {
    // Three corrupt warm lines ahead of one readable hot line.
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    sb.push_str("HotLine");
    sb.inject_corrupted_warm_block(3);
    assert_eq!(sb.line_count(), 4);
    assert!(
        sb.get_line_rev(1).is_err(),
        "the fixture needs a corrupt line"
    );

    let sb_before = sb.line_count();
    let mut grid = Grid::with_tiered_scrollback(4, 20, 0, sb);

    // Unscroll 4: the hot line reads, the three corrupt ones fail, so unscroll
    // aborts with 0 and leaves grid and scrollback unchanged.
    let unscrolled = grid.unscroll_from_scrollback(4);
    assert_eq!(unscrolled, 0, "Unscroll must abort when any line fails");
    assert_eq!(
        grid.tiered_scrollback_lines(),
        sb_before,
        "Scrollback line count must be unchanged after abort"
    );
}
