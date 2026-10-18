// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the link/search-translation accessors exposed to wasm:
//! `base_y`, `display_origin_absolute`, `row_is_wrapped`, `row_len`.

use super::super::*;

#[test]
fn display_origin_absolute_tracks_scroll() {
    let mut grid = Grid::with_scrollback(3, 80, 100);
    for i in 0..10 {
        grid.write_char((b'A' + i) as char);
        grid.line_feed();
    }

    assert_eq!(grid.display_offset(), 0);
    assert_eq!(grid.display_origin_absolute(), grid.base_y());

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
    assert_eq!(grid.row_is_wrapped(0), Some(false));
    assert_eq!(grid.row_is_wrapped(1), Some(true));
    assert_eq!(grid.row_is_wrapped(999), None);
}

#[test]
fn row_len_is_logical_length() {
    let mut grid = Grid::new(24, 80);
    for c in "abc".chars() {
        grid.write_char(c);
    }
    assert_eq!(grid.row_len(0), Some(3));
    assert_eq!(grid.row_len(1), Some(0));
    assert_eq!(grid.row_len(999), None);
}

#[test]
fn scrolled_history_wrapped_row_metadata_is_tier_aware() {
    // A scrolled-back history row past the ring base (where `Grid::row` is None) reports
    // `row_len`/`row_is_wrapped` through the tier-aware view, as `row_text` does.
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 40, 2, scrollback);
    // Three 30-char lines fill the screen with nothing scrolled off yet.
    for (i, ch) in ['A', 'B', 'C'].into_iter().enumerate() {
        for _ in 0..30 {
            grid.write_char(ch);
        }
        if i < 2 {
            grid.line_feed();
        }
    }
    // Each 30-char line rewraps to a 20-col head plus a 10-col continuation, overflowing into
    // history.
    grid.resize(3, 20);
    grid.scroll_to_top();
    assert!(grid.display_offset() > 0, "scrolled into history");

    assert!(
        grid.row(0).is_none(),
        "row 0 is past the ring base (history)"
    );
    assert!(
        grid.row(1).is_none(),
        "row 1 is past the ring base (history)"
    );
    assert_eq!(grid.row_text(0).as_deref(), Some("A".repeat(20).as_str()));
    assert_eq!(grid.row_text(1).as_deref(), Some("A".repeat(10).as_str()));

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
    assert_eq!(grid.row_len(999), None);
    assert_eq!(grid.row_is_wrapped(999), None);
}
