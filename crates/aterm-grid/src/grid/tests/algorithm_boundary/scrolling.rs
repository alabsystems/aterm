// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Line feed / reverse line feed with scroll regions.
//!
//! Migrated from aterm-core as part of #6556 Batch 2.

use super::*;

#[test]
fn line_feed_within_scroll_region_moves_down() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(2, 7);
    grid.set_cursor(4, 5);

    grid.line_feed();

    assert_eq!(grid.cursor_row(), 5);
    assert_eq!(grid.cursor_col(), 5);
}

#[test]
fn line_feed_at_bottom_margin_scrolls_region() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(2, 5);

    for row in 2..=5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + (row - 2) as u8) as char);
    }

    grid.set_cursor(5, 0);
    grid.line_feed();

    assert_eq!(grid.cursor_row(), 5);
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'D');
    assert!(
        grid.row(5).unwrap().is_empty(),
        "bottom of region should be blank after scroll"
    );
    assert!(grid.row(0).unwrap().is_empty());
    assert!(grid.row(1).unwrap().is_empty());
}

#[test]
fn line_feed_below_scroll_region_moves_down() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(2, 5);
    grid.set_cursor(7, 3);

    grid.line_feed();

    assert_eq!(grid.cursor_row(), 8);
    assert_eq!(grid.cursor_col(), 3);
}

#[test]
fn line_feed_below_region_clamped_at_screen_bottom() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(2, 5);
    grid.set_cursor(9, 0);

    grid.line_feed();

    assert_eq!(grid.cursor_row(), 9);
}

#[test]
fn line_feed_above_scroll_region_moves_down() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(5, 8);
    grid.set_cursor(1, 0);

    grid.line_feed();

    assert_eq!(grid.cursor_row(), 2);
}

#[test]
fn reverse_line_feed_within_scroll_region_moves_up() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(2, 7);
    grid.set_cursor(5, 3);

    grid.reverse_line_feed();

    assert_eq!(grid.cursor_row(), 4);
    assert_eq!(grid.cursor_col(), 3);
}

#[test]
fn reverse_line_feed_at_top_margin_scrolls_region_down() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(2, 5);

    for row in 2..=5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + (row - 2) as u8) as char);
    }

    grid.set_cursor(2, 0);
    grid.reverse_line_feed();

    assert_eq!(grid.cursor_row(), 2);
    assert!(
        grid.row(2).unwrap().is_empty(),
        "top of region should be blank after reverse scroll"
    );
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'C');
}

#[test]
fn reverse_line_feed_above_scroll_region_moves_up() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(5, 8);
    grid.set_cursor(2, 0);

    grid.reverse_line_feed();

    assert_eq!(grid.cursor_row(), 1);
}

#[test]
fn reverse_line_feed_above_region_clamped_at_row_zero() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(5, 8);
    grid.set_cursor(0, 0);

    grid.reverse_line_feed();

    assert_eq!(grid.cursor_row(), 0);
}

#[test]
fn single_row_scroll_region_rejected_per_vt510() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(3, 3);

    let region = grid.scroll_region();
    assert!(
        region.is_full(grid.rows()),
        "set_scroll_region(3,3) should reset to full screen, got top={} bottom={}",
        region.top,
        region.bottom,
    );

    grid.set_cursor(4, 0);
    grid.line_feed();
    assert_eq!(
        grid.cursor_row(),
        5,
        "cursor should move down (full-screen region), not stay at 4"
    );
    grid.assert_invariants();
}

#[test]
fn two_row_scroll_region_line_feed() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(3, 4);

    grid.set_cursor(3, 0);
    grid.write_char('A');
    grid.set_cursor(4, 0);
    grid.write_char('B');

    grid.set_cursor(4, 0);
    grid.line_feed();

    assert_eq!(grid.cursor_row(), 4);
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'B');
    assert!(grid.row(4).unwrap().is_empty());
}

#[test]
fn two_row_scroll_region_reverse_line_feed() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(3, 4);

    grid.set_cursor(3, 0);
    grid.write_char('A');
    grid.set_cursor(4, 0);
    grid.write_char('B');

    grid.set_cursor(3, 0);
    grid.reverse_line_feed();

    assert_eq!(grid.cursor_row(), 3);
    assert!(grid.row(3).unwrap().is_empty());
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'A');
}

#[test]
fn write_char_wrap_at_scroll_region_bottom_scrolls_region_only() {
    let mut grid = build_autowrap_scroll_region_fixture();
    grid.set_cursor(3, 0);

    for c in "XXXXX".chars() {
        grid.write_char_wrap(c);
    }
    grid.write_char_wrap('Z');

    assert_rows_outside_scroll_region_preserved(&grid);
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'Z');
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn write_char_wrap_styled_at_scroll_region_bottom_scrolls_region_only() {
    let mut grid = build_autowrap_scroll_region_fixture();
    grid.set_cursor(3, 0);

    let fg = PackedColor::indexed(196);
    let bg = PackedColor::indexed(22);
    let flags = CellFlags::BOLD;
    for c in "XXXXX".chars() {
        grid.write_char_wrap_styled(c, fg, bg, flags);
    }
    grid.write_char_wrap_styled('Z', fg, bg, flags);

    assert_rows_outside_scroll_region_preserved(&grid);
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'Z');
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn write_ascii_blast_at_scroll_region_bottom_scrolls_region_only() {
    let mut grid = build_autowrap_scroll_region_fixture();
    grid.set_cursor(3, 0);

    let written = grid.write_ascii_blast(b"XXXXXZ");
    assert_eq!(written, 6);

    assert_rows_outside_scroll_region_preserved(&grid);
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'Z');
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn write_ascii_run_styled_at_scroll_region_bottom_scrolls_region_only() {
    let mut grid = build_autowrap_scroll_region_fixture();
    grid.set_cursor(3, 0);

    let fg = PackedColor::indexed(33);
    let bg = PackedColor::indexed(235);
    let flags = CellFlags::ITALIC;
    let mut last_byte = None;
    let written = grid.write_ascii_run_styled(b"XXXXXZ", fg, bg, flags, &mut last_byte);
    assert_eq!(written, 6);
    assert_eq!(last_byte, Some(b'Z'));

    assert_rows_outside_scroll_region_preserved(&grid);
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'Z');
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn write_wide_char_wrap_at_scroll_region_bottom_scrolls_region_only() {
    let mut grid = build_autowrap_scroll_region_fixture();

    grid.set_cursor(3, 4);
    let ok = grid.write_wide_char_wrap_styled(
        '好',
        PackedColor::indexed(45),
        PackedColor::indexed(232),
        CellFlags::BOLD,
    );
    assert!(ok);

    assert_rows_outside_scroll_region_preserved(&grid);
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(3, 0).unwrap().char(), '好');
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.cursor_col(), 2);
}

// Full-screen region scrolls with scrollback
// ========================================================================

#[test]
fn line_feed_full_screen_region_scrolls_with_scrollback() {
    let mut grid = Grid::with_scrollback(3, 5, 10);

    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.set_cursor(2, 0);
    grid.write_char('C');

    grid.set_cursor(2, 0);
    grid.line_feed();

    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
    assert!(grid.row(2).unwrap().is_empty());
}

// Algorithm audit: scroll region boundary conditions
// ========================================================================

/// Scroll region up with n == region_size clears all rows in the region.
#[test]
fn scroll_region_up_n_equals_region_size() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.set_scroll_region(1, 3);
    grid.scroll_region_up(3);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A', "row 0 preserved");
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'E', "row 4 preserved");
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'F', "row 5 preserved");

    for row in 1..=3 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be empty after scrolling entire region up",
        );
    }
}

/// Scroll region down with n == region_size clears all rows in the region.
#[test]
fn scroll_region_down_n_equals_region_size() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.set_scroll_region(1, 3);
    grid.scroll_region_down(3);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A', "row 0 preserved");
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'E', "row 4 preserved");
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'F', "row 5 preserved");

    for row in 1..=3 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be empty after scrolling entire region down",
        );
    }
}

/// Scroll region up with n > region_size is clamped to region_size.
#[test]
fn scroll_region_up_n_exceeds_region_size() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.set_scroll_region(1, 3);
    grid.scroll_region_up(100);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A', "row 0 preserved");
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'E', "row 4 preserved");

    for row in 1..=3 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be empty after oversized scroll",
        );
    }
}

/// Two-row scroll region: scroll up by 1 shifts content within region only.
#[test]
fn scroll_region_up_two_row() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_scroll_region(2, 3);
    grid.scroll_region_up(1);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A', "row 0 preserved");
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B', "row 1 preserved");
    assert_eq!(
        grid.cell(2, 0).unwrap().char(),
        'D',
        "row 3 content shifts to row 2"
    );
    assert!(
        grid.row(3).unwrap().is_empty(),
        "bottom of region should be cleared"
    );
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'E', "row 4 preserved");
}

/// advance_autowrap_line with cursor below scroll region at screen bottom
/// must NOT scroll — xterm xtermIndex with `cur_row > bot_marg` is
/// CursorDown, which clamps at `max_row`. The wrap only returns the cursor
/// to column 0 of the last row, where output keeps overwriting in place.
#[test]
fn write_char_wrap_below_scroll_region_at_screen_bottom_does_not_scroll() {
    let mut grid = Grid::with_scrollback(5, 5, 10);

    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(1, 3);
    grid.set_cursor(4, 4);

    grid.write_char_wrap('Z');
    grid.resolve_pending_wrap();

    assert_eq!(
        grid.scrollback_lines(),
        0,
        "no scrollback: display must not scroll"
    );
    for (row, ch) in ['A', 'B', 'C', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(
            grid.cell(row as u16, 0).unwrap().char(),
            ch,
            "row {row} content untouched by wrap below the region"
        );
    }
    assert_eq!(grid.cell(4, 4).unwrap().char(), 'Z', "'Z' written at (4,4)");
    assert_eq!(grid.cursor_row(), 4, "cursor stays on the bottom row");
    assert_eq!(grid.cursor_col(), 0, "cursor at col 0 after wrap");
    grid.assert_invariants();
}

/// Regression test for #5019: scroll_display followed by line_feed at bottom
/// of scroll region must not panic.
#[test]
fn line_feed_after_scroll_display_resets_offset() {
    let mut grid = Grid::with_scrollback(10, 10, 20);

    for row in 0..10 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_cursor(9, 0);
    for _ in 0..5 {
        grid.line_feed();
    }
    assert!(grid.scrollback_lines() > 0, "should have scrollback");

    grid.set_scroll_region(2, 7);
    grid.set_cursor(7, 0);

    grid.scroll_display(3);
    assert!(grid.display_offset() > 0, "should be scrolled back");

    grid.line_feed();

    assert_eq!(grid.display_offset(), 0, "should snap to live view");
    assert_eq!(grid.cursor_row(), 7, "cursor stays at bottom of region");
    grid.assert_invariants();
}

/// Same as above but for reverse_line_feed + scroll_region_down.
#[test]
fn reverse_line_feed_after_scroll_display_resets_offset() {
    let mut grid = Grid::with_scrollback(10, 10, 20);

    for row in 0..10 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_cursor(9, 0);
    for _ in 0..5 {
        grid.line_feed();
    }

    grid.set_scroll_region(2, 7);
    grid.set_cursor(2, 0);

    grid.scroll_display(3);
    assert!(grid.display_offset() > 0, "should be scrolled back");

    grid.reverse_line_feed();

    assert_eq!(grid.display_offset(), 0, "should snap to live view");
    assert_eq!(grid.cursor_row(), 2, "cursor stays at top of region");
    grid.assert_invariants();
}

/// Regression test for #5019: write_char_wrap at end of line in a scroll
/// region with nonzero display_offset must not panic.
#[test]
fn write_char_wrap_after_scroll_display_resets_offset() {
    let mut grid = Grid::with_scrollback(10, 10, 20);

    for row in 0..10 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_cursor(9, 0);
    for _ in 0..5 {
        grid.line_feed();
    }

    grid.set_scroll_region(2, 7);
    grid.set_cursor(7, 9);

    grid.scroll_display(3);
    assert!(grid.display_offset() > 0, "should be scrolled back");

    grid.write_char_wrap('X');
    grid.resolve_pending_wrap();

    assert_eq!(grid.display_offset(), 0, "should snap to live view");
    grid.assert_invariants();
}

/// Regression test for #5019: scroll_region_up called directly with nonzero
/// display_offset must self-heal.
#[test]
fn scroll_region_up_direct_with_nonzero_display_offset() {
    let mut grid = Grid::with_scrollback(10, 10, 20);

    for row in 0..10 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_cursor(9, 0);
    for _ in 0..5 {
        grid.line_feed();
    }

    grid.set_scroll_region(2, 7);
    grid.scroll_display(3);
    assert!(grid.display_offset() > 0);

    grid.scroll_region_up(1);
    assert_eq!(grid.display_offset(), 0);
    grid.assert_invariants();
}

/// set_scroll_region with top == bottom falls back to full screen.
#[test]
fn scroll_region_degenerate_top_equals_bottom() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_scroll_region(2, 2);

    assert!(
        grid.scroll_region().is_full(5),
        "degenerate region should fall back to full screen"
    );
}

// ========================================================================
// Targeted damage: mark_rows instead of mark_full (#5561)
// ========================================================================

/// scroll_to_bottom with small display_offset marks only bottom rows dirty.
#[test]
fn scroll_to_bottom_targeted_damage_small_offset() {
    let mut grid = Grid::with_scrollback(24, 80, 100);

    for i in 0..30u16 {
        grid.set_cursor(23, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }

    grid.scroll_display(3);
    assert_eq!(grid.display_offset(), 3);

    grid.clear_damage();
    grid.scroll_to_bottom();
    assert_eq!(grid.display_offset(), 0);

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "targeted damage: scroll_to_bottom with offset=3 should not mark_full on a 24-row grid"
    );

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    let dirty_rows: Vec<u16> = bounds.iter().map(|b| b.line).collect();
    assert_eq!(
        dirty_rows,
        vec![21, 22, 23],
        "expected exactly rows 21-23 dirty, got {dirty_rows:?}"
    );
}

/// scroll_to_bottom with offset >= visible_rows falls back to mark_full.
#[test]
fn scroll_to_bottom_full_damage_large_offset() {
    let mut grid = Grid::with_scrollback(5, 10, 100);

    for i in 0..20u16 {
        grid.set_cursor(4, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }

    grid.scroll_to_top();
    assert!(grid.display_offset() >= 5);

    grid.clear_damage();
    grid.scroll_to_bottom();

    assert!(grid.damage().has_damage());
    assert!(
        grid.damage().is_full(),
        "scroll_to_bottom with offset >= visible_rows should mark_full"
    );
}

/// scroll_region_up marks only scroll region rows dirty (not full).
#[test]
fn scroll_region_up_targeted_damage() {
    let mut grid = Grid::new(24, 80);
    grid.set_scroll_region(5, 15);

    for row in 5..=15 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + (row - 5) as u8) as char);
    }

    grid.clear_damage();
    grid.scroll_region_up(1);

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "scroll_region_up should use targeted damage, not mark_full"
    );

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    for b in &bounds {
        assert!(
            b.line >= 5 && b.line <= 15,
            "dirty row {} should be within scroll region [5, 15]",
            b.line
        );
    }
}

/// erase_to_end_of_screen marks only rows below cursor (not full).
#[test]
fn erase_to_end_of_screen_targeted_damage() {
    let mut grid = Grid::new(24, 80);

    for row in 0..24u16 {
        grid.set_cursor(row, 0);
        grid.write_char('X');
    }

    grid.set_cursor(20, 0);
    grid.clear_damage();
    grid.erase_to_end_of_screen();

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "erase_to_end_of_screen from row 20 should use targeted damage"
    );

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    for b in &bounds {
        assert!(
            b.line >= 20,
            "dirty row {} should be at or below cursor row 20",
            b.line
        );
    }
}

// ========================================================================
// Mixed Phase 1 + Phase 2 scroll_up boundary (#4335 algorithm_audit)
// ========================================================================

/// A single scroll_up(n) call where n > rows_until_capacity exercises both
/// Phase 1 (growth) and Phase 2 (reuse) in the same invocation.
#[test]
fn scroll_up_mixed_phase1_phase2_single_call() {
    let mut grid = Grid::with_scrollback(4, 10, 3);

    fill_grid_rows(&mut grid, 4);

    grid.scroll_up(2);
    assert_eq!(
        grid.cell(0, 0).unwrap().char(),
        'C',
        "after scroll(2): row 0 = old row 2"
    );
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'D',
        "after scroll(2): row 1 = old row 3"
    );
    assert!(
        grid.row(2).unwrap().is_empty(),
        "after scroll(2): row 2 blank"
    );
    assert!(
        grid.row(3).unwrap().is_empty(),
        "after scroll(2): row 3 blank"
    );

    grid.set_cursor(2, 0);
    grid.write_char('E');
    grid.set_cursor(3, 0);
    grid.write_char('F');

    grid.scroll_up(3);

    assert_eq!(
        grid.cell(0, 0).unwrap().char(),
        'F',
        "mixed scroll: row 0 = old row 3 ('F')"
    );
    assert!(grid.row(1).unwrap().is_empty(), "mixed scroll: row 1 blank");
    assert!(grid.row(2).unwrap().is_empty(), "mixed scroll: row 2 blank");
    assert!(grid.row(3).unwrap().is_empty(), "mixed scroll: row 3 blank");

    let ring_sb = grid.storage.ring_buffer_scrollback();
    let extras_len = grid.storage.ring_extras.len();
    assert_eq!(
        extras_len, ring_sb,
        "ring_extras.len()={extras_len} != ring_buffer_scrollback()={ring_sb}"
    );

    grid.assert_invariants();
}

/// Mixed-phase scroll_up with tiered scrollback content verification.
#[test]
fn scroll_up_mixed_phase_scrollback_content() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 10, 2, scrollback);

    fill_grid_rows(&mut grid, 3);

    grid.scroll_up(1);

    grid.set_cursor(2, 0);
    grid.write_char('D');

    grid.scroll_up(3);

    assert!(
        grid.row(0).unwrap().is_empty(),
        "row 0 blank after mixed scroll"
    );
    assert!(
        grid.row(1).unwrap().is_empty(),
        "row 1 blank after mixed scroll"
    );
    assert!(
        grid.row(2).unwrap().is_empty(),
        "row 2 blank after mixed scroll"
    );

    // Lines may be in the lazy buffer (deferred) or tiered scrollback.
    // Check the combined count: tiered_scrollback_lines() includes both.
    let line_count = grid.tiered_scrollback_lines();
    assert!(
        line_count >= 2,
        "scrollback should have at least 2 lines from Phase 2 eviction, got {line_count}"
    );

    grid.assert_invariants();
}

// ========================================================================
// Degenerate single-row scroll region: top == bottom (#7751)
// ========================================================================

/// scroll_region_up with top == bottom must be a no-op: line content preserved.
#[test]
fn scroll_region_up_top_equals_bottom_is_noop() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    // Bypass set_scroll_region validation to create a degenerate region
    // where top == bottom. Normal DECSTBM rejects this, but direct callers
    // of scroll_region_up could hit it with programmatic regions.
    grid.storage.cursor_state.scroll_region = ScrollRegion { top: 3, bottom: 3 };

    grid.scroll_region_up(1);

    // All rows must be preserved — the degenerate region is a no-op.
    for row in 0..6u16 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "row {row} should be unchanged after degenerate scroll_region_up",
        );
    }
    grid.assert_invariants();
}

/// scroll_region_down with top == bottom must be a no-op: line content preserved.
#[test]
fn scroll_region_down_top_equals_bottom_is_noop() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.storage.cursor_state.scroll_region = ScrollRegion { top: 3, bottom: 3 };

    grid.scroll_region_down(1);

    for row in 0..6u16 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "row {row} should be unchanged after degenerate scroll_region_down",
        );
    }
    grid.assert_invariants();
}

/// scroll_region_up_margined with top == bottom must be a no-op.
#[test]
fn scroll_region_up_margined_top_equals_bottom_is_noop() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.storage.cursor_state.scroll_region = ScrollRegion { top: 3, bottom: 3 };

    grid.scroll_region_up_margined(1, 2, 5);

    for row in 0..6u16 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "row {row} should be unchanged after degenerate scroll_region_up_margined",
        );
    }
    grid.assert_invariants();
}

/// scroll_region_down_margined with top == bottom must be a no-op.
#[test]
fn scroll_region_down_margined_top_equals_bottom_is_noop() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.storage.cursor_state.scroll_region = ScrollRegion { top: 3, bottom: 3 };

    grid.scroll_region_down_margined(1, 2, 5);

    for row in 0..6u16 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "row {row} should be unchanged after degenerate scroll_region_down_margined",
        );
    }
    grid.assert_invariants();
}

/// Normal scroll_region_up with top < bottom still works (regression check).
#[test]
fn scroll_region_up_normal_still_works() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.set_scroll_region(2, 4);
    grid.scroll_region_up(1);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A', "row 0 preserved");
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B', "row 1 preserved");
    assert_eq!(
        grid.cell(2, 0).unwrap().char(),
        'D',
        "row 3 shifted to row 2"
    );
    assert_eq!(
        grid.cell(3, 0).unwrap().char(),
        'E',
        "row 4 shifted to row 3"
    );
    assert!(grid.row(4).unwrap().is_empty(), "bottom of region cleared");
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'F', "row 5 preserved");
    grid.assert_invariants();
}

/// Normal scroll_region_down with top < bottom still works (regression check).
#[test]
fn scroll_region_down_normal_still_works() {
    let mut grid = Grid::new(6, 10);
    fill_grid_rows(&mut grid, 6);

    grid.set_scroll_region(2, 4);
    grid.scroll_region_down(1);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A', "row 0 preserved");
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B', "row 1 preserved");
    assert!(grid.row(2).unwrap().is_empty(), "top of region cleared");
    assert_eq!(
        grid.cell(3, 0).unwrap().char(),
        'C',
        "row 2 shifted to row 3"
    );
    assert_eq!(
        grid.cell(4, 0).unwrap().char(),
        'D',
        "row 3 shifted to row 4"
    );
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'F', "row 5 preserved");
    grid.assert_invariants();
}
