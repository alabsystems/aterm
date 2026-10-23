// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending wrap (`do_wrap`) and `wrap_serial` conformance tests.

use super::super::*;

fn grid_with_pending_wrap(rows: u16, cols: u16) -> Grid {
    let mut grid = Grid::new(rows, cols);
    grid.set_cursor(0, 0);
    for col in 0..cols {
        grid.write_char_wrap((b'A' + (col % 26) as u8) as char);
    }
    assert!(
        grid.pending_wrap(),
        "precondition: pending_wrap must be set after writing to last column"
    );
    assert_eq!(grid.cursor_col(), cols - 1, "cursor at last column");
    grid
}

/// IL/DL (within region and margins) and LF consume `pending_wrap`, while SU/SD, HT, and CBT keep it.
#[test]
fn each_operation_keeps_or_consumes_the_parked_wrap() {
    type Op = fn(&mut Grid);
    let cases: &[(&str, u16, u16, Op, bool)] = &[
        ("IL", 5, 5, |g| g.insert_lines(1), false),
        ("DL", 5, 5, |g| g.delete_lines(1), false),
        (
            "IL in the region",
            5,
            5,
            |g| {
                g.set_scroll_region(0, 4);
                g.insert_lines(1);
            },
            false,
        ),
        (
            "IL below the region",
            5,
            5,
            |g| {
                g.set_scroll_region(1, 4);
                g.insert_lines(1);
            },
            true,
        ),
        (
            "DL below the region",
            5,
            5,
            |g| {
                g.set_scroll_region(1, 4);
                g.delete_lines(1);
            },
            true,
        ),
        (
            "IL inside the margins",
            5,
            5,
            |g| {
                g.set_horizontal_margins(2, 4);
                g.insert_lines_margined(1, true);
            },
            false,
        ),
        (
            "IL outside the margins",
            5,
            5,
            |g| {
                g.set_horizontal_margins(0, 2);
                g.insert_lines_margined(1, true);
            },
            true,
        ),
        (
            "DL inside the margins",
            5,
            5,
            |g| {
                g.set_horizontal_margins(2, 4);
                g.delete_lines_margined(1, true);
            },
            false,
        ),
        (
            "DL outside the margins",
            5,
            5,
            |g| {
                g.set_horizontal_margins(0, 2);
                g.delete_lines_margined(1, true);
            },
            true,
        ),
        ("SU", 5, 5, |g| g.scroll_region_up(1), true),
        ("SD", 5, 5, |g| g.scroll_region_down(1), true),
        ("full-screen scroll", 5, 5, |g| g.scroll_up(1), true),
        (
            "moving LF",
            3,
            5,
            |g| {
                g.set_scroll_region(0, 2);
                g.line_feed();
            },
            false,
        ),
        ("scrolling LF", 1, 5, Grid::line_feed, false),
        ("HT", 3, 20, Grid::tab, true),
        ("CBT", 3, 20, Grid::back_tab, true),
        (
            "margin-aware CBT",
            3,
            20,
            |g| {
                g.set_horizontal_margins(4, 19);
                g.back_tab_margin(true, true);
            },
            true,
        ),
    ];
    for &(name, rows, cols, op, keeps) in cases {
        let mut grid = grid_with_pending_wrap(rows, cols);
        op(&mut grid);
        assert_eq!(grid.pending_wrap(), keeps, "{name}");
    }
    let mut grid = grid_with_pending_wrap(3, 20);
    grid.back_tab();
    assert_eq!(
        grid.cursor_col(),
        16,
        "CBT still moves to the previous stop"
    );
}

/// `wrap_serial` increments once per resolved autowrap and ignores hard newlines.
#[test]
fn wrap_serial_counts_resolved_wraps_and_ignores_hard_newlines() {
    let mut grid = grid_with_pending_wrap(3, 5);
    assert_eq!(
        grid.wrap_serial(),
        0,
        "a parked deferred wrap is not yet a wrap"
    );

    grid.write_char_wrap('f');
    assert_eq!(grid.wrap_serial(), 1, "one wrapped line, one bump");
    assert_eq!(grid.cursor_row(), 1, "the resolve advanced the line");

    grid.write_char_wrap('g');
    assert_eq!(grid.wrap_serial(), 1, "mid-line typing is not a wrap");

    grid.carriage_return();
    grid.line_feed();
    assert_eq!(grid.wrap_serial(), 1, "a hard newline is not a wrap");

    for col in 0..5 {
        grid.write_char_wrap((b'A' + col as u8) as char);
    }
    assert_eq!(grid.wrap_serial(), 1, "the second wrap is still parked");
    grid.write_char_wrap('h');
    assert_eq!(grid.wrap_serial(), 2, "second wrapped line, second bump");
}
