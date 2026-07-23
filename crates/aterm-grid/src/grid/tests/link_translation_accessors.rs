// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the link/search-translation accessors exposed to wasm:
//! `base_y`, `display_origin_absolute`, `row_is_wrapped`, `row_len`.

use super::super::*;

#[test]
fn base_y_equals_oldest_absolute_plus_scrollback() {
    let mut grid = Grid::with_scrollback(3, 80, 100);
    for i in 0..10 {
        grid.write_char((b'A' + i) as char);
        grid.line_feed();
    }

    assert!(
        grid.scrollback_lines() > 0,
        "lines should have scrolled off"
    );
    // base_y is the absolute row of the live/last line.
    let expected = grid.oldest_absolute_row() as usize + grid.scrollback_lines();
    assert_eq!(grid.base_y(), expected);
}

#[test]
fn display_origin_absolute_tracks_scroll() {
    let mut grid = Grid::with_scrollback(3, 80, 100);
    for i in 0..10 {
        grid.write_char((b'A' + i) as char);
        grid.line_feed();
    }

    // Not scrolled: top visible == base_y.
    assert_eq!(grid.display_offset(), 0);
    assert_eq!(grid.display_origin_absolute(), grid.base_y());

    // Scroll up two lines into history: origin drops by exactly the offset.
    grid.scroll_display(2);
    assert_eq!(grid.display_offset(), 2);
    assert_eq!(grid.display_origin_absolute(), grid.base_y() - 2);
}

#[test]
fn row_is_wrapped_true_on_continuation_row() {
    let mut grid = Grid::new(24, 5);
    for c in "Hello World".chars() {
        grid.write_char_wrap(c);
    }
    // Row 0 is the lead; row 1 is the soft-wrap continuation.
    assert_eq!(grid.row_is_wrapped(0), Some(false));
    assert_eq!(grid.row_is_wrapped(1), Some(true));
    // Out-of-range row yields None.
    assert_eq!(grid.row_is_wrapped(999), None);
}

#[test]
fn row_len_is_logical_length() {
    let mut grid = Grid::new(24, 80);
    for c in "abc".chars() {
        grid.write_char(c);
    }
    // Last non-empty cell + 1.
    assert_eq!(grid.row_len(0), Some(3));
    // A never-written row is blank (len 0).
    assert_eq!(grid.row_len(1), Some(0));
    assert_eq!(grid.row_len(999), None);
}

#[test]
fn scrolled_history_wrapped_row_metadata_is_tier_aware() {
    // P1: after a ring-only width-shrink reflow overflows wrapped rows into the
    // lazy buffer, a scrolled-back HISTORY row (past the ring base, where
    // Grid::row is None) returned correct row_text — which routes through the
    // tier-aware visible_row_view — but pre-fix row_len/row_is_wrapped resolved
    // straight through Grid::row and so reported None for the same rows.
    let mut grid = Grid::new(3, 40); // ring-only: 10k ring, no tiered store
    // Three 30-char lines fill the 3 visible rows without wrapping at 40 cols;
    // the last has no line feed so the cursor rests on the bottom row.
    for (i, ch) in ['A', 'B', 'C'].into_iter().enumerate() {
        for _ in 0..30 {
            grid.write_char(ch);
        }
        if i < 2 {
            grid.line_feed();
        }
    }
    // Width shrink to 20: each 30-char line rewraps to a 20-col head + a 10-col
    // WRAPPED continuation, overflowing the 3-row window; the top rows spill to
    // the lazy buffer (the only place ring-only reflow overflow lands).
    grid.resize(3, 20);
    grid.scroll_to_top();
    assert!(grid.display_offset() > 0, "scrolled into history");

    // The two oldest viewport rows are HISTORY rows: Grid::row is None there.
    assert!(
        grid.row(0).is_none(),
        "row 0 is past the ring base (history)"
    );
    assert!(
        grid.row(1).is_none(),
        "row 1 is past the ring base (history)"
    );
    // Text is already tier-aware (the sibling that always worked).
    assert_eq!(grid.row_text(0).as_deref(), Some("A".repeat(20).as_str()));
    assert_eq!(grid.row_text(1).as_deref(), Some("A".repeat(10).as_str()));

    // The metadata pair is now tier-aware too (non-None, matching the content).
    assert_eq!(grid.row_len(0), Some(20), "history head-row length");
    assert_eq!(
        grid.row_is_wrapped(0),
        Some(false),
        "history head not wrapped"
    );
    assert_eq!(grid.row_len(1), Some(10), "history continuation length");
    assert_eq!(
        grid.row_is_wrapped(1),
        Some(true),
        "history continuation is a wrap continuation"
    );
    // Out-of-range still yields None.
    assert_eq!(grid.row_len(999), None);
    assert_eq!(grid.row_is_wrapped(999), None);
}
