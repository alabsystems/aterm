// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `scroll_display` targeted damage patterns (#5227)
//! and `content_scroll_delta` consumption (#5544).

use super::*;

/// scroll_display up by small delta marks only top N rows dirty.
#[test]
fn scroll_display_up_marks_top_rows_dirty() {
    let mut grid = Grid::with_scrollback(10, 10, 50);

    // Generate scrollback
    for _ in 0..20 {
        grid.set_cursor(9, 0);
        grid.write_char('X');
        grid.line_feed();
    }
    assert!(grid.scrollback_lines() >= 3);

    grid.clear_damage();
    grid.scroll_display(3); // scroll up = show older content
    assert_eq!(grid.display_offset(), 3);

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "scroll_display(3) on a 10-row grid should use targeted damage"
    );

    // Top 3 rows (0, 1, 2) should be dirty — these show newly exposed scrollback
    for row in 0..3u16 {
        assert!(
            grid.damage().is_row_damaged(row),
            "row {row} should be dirty after scroll_display up by 3"
        );
    }
    // Rows below the scroll delta should NOT be dirty
    for row in 3..10u16 {
        assert!(
            !grid.damage().is_row_damaged(row),
            "row {row} should NOT be dirty after scroll_display up by 3"
        );
    }
}

/// scroll_display down by small delta marks only bottom N rows dirty.
#[test]
fn scroll_display_down_marks_bottom_rows_dirty() {
    let mut grid = Grid::with_scrollback(10, 10, 50);

    // Generate scrollback
    for _ in 0..20 {
        grid.set_cursor(9, 0);
        grid.write_char('X');
        grid.line_feed();
    }

    // First scroll up, then scroll back down
    grid.scroll_display(5);
    assert_eq!(grid.display_offset(), 5);

    grid.clear_damage();
    grid.scroll_display(-3); // scroll down = show newer content
    assert_eq!(grid.display_offset(), 2);

    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "scroll_display(-3) on a 10-row grid should use targeted damage"
    );

    // Bottom 3 rows (7, 8, 9) should be dirty — newly exposed live content
    for row in 7..10u16 {
        assert!(
            grid.damage().is_row_damaged(row),
            "row {row} should be dirty after scroll_display down by 3"
        );
    }
    // Rows above the scroll delta should NOT be dirty
    for row in 0..7u16 {
        assert!(
            !grid.damage().is_row_damaged(row),
            "row {row} should NOT be dirty after scroll_display down by 3"
        );
    }
}

/// scroll_display with delta >= visible_rows falls back to full damage.
#[test]
fn scroll_display_large_delta_marks_full_damage() {
    let mut grid = Grid::with_scrollback(5, 10, 100);

    // Generate enough scrollback
    for _ in 0..30 {
        grid.set_cursor(4, 0);
        grid.write_char('X');
        grid.line_feed();
    }

    grid.clear_damage();
    grid.scroll_display(10); // delta >= rows(5) → full damage

    assert!(grid.damage().has_damage());
    assert!(
        grid.damage().is_full(),
        "scroll_display with delta >= visible_rows should mark_full"
    );
}

/// scroll_display clamped to bounds produces no damage when already at limit.
#[test]
fn scroll_display_clamped_no_damage() {
    let mut grid = Grid::with_scrollback(5, 10, 50);

    // Generate scrollback
    for _ in 0..10 {
        grid.set_cursor(4, 0);
        grid.write_char('X');
        grid.line_feed();
    }

    // Scroll to max
    grid.scroll_to_top();
    let max = grid.display_offset();

    grid.clear_damage();
    // Try to scroll further up — clamped, no actual movement
    grid.scroll_display(100);
    assert_eq!(grid.display_offset(), max);

    // No damage because offset didn't change
    assert!(
        !grid.damage().has_damage(),
        "scroll_display clamped at max should produce no damage"
    );
}

/// scroll_display with delta=0 produces no damage.
#[test]
fn scroll_display_zero_delta_no_damage() {
    let mut grid = Grid::with_scrollback(5, 10, 50);

    for _ in 0..10 {
        grid.set_cursor(4, 0);
        grid.write_char('X');
        grid.line_feed();
    }

    grid.scroll_display(3);
    grid.clear_damage();
    grid.scroll_display(0);

    assert!(
        !grid.damage().has_damage(),
        "scroll_display(0) should produce no damage"
    );
}

/// content_scroll_delta is set by scroll_up and consumed by take.
#[test]
fn content_scroll_delta_set_by_scroll_up_and_consumed() {
    let mut grid = Grid::with_scrollback(5, 10, 50);

    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char('X');
    }
    grid.clear_damage();

    // Trigger scroll
    grid.set_cursor(4, 0);
    grid.line_feed();

    // content_scroll_delta accumulates the shift, then take resets it.
    let content_delta = grid.take_content_scroll_delta();
    assert_eq!(
        content_delta, 1,
        "scroll_up(1) should set content_scroll_delta"
    );
    assert_eq!(
        grid.take_content_scroll_delta(),
        0,
        "take_content_scroll_delta should return 0 after consumption"
    );
}
