// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending wrap conformance tests.
//!
//! Which operations clear `pending_wrap` (xterm's `do_wrap`) and which keep it.
//! Per xterm (`util.c` / `charproc.c`) these clear it: ICH/DCH, IL/DL once their
//! margin tests pass, ECH, ED 0/1/2 and EL 0/1/2, the selective DECSEL/DECSED
//! erases (unless every cell of the span is protected, except DECSEL 0 and
//! DECSED 0 away from the origin, which always clear), DECALN, and CSI ?1049 h.
//! SU/SD, TAB and CBT, ED 3 and the rectangle ops keep it.

use super::super::*;

/// A grid with pending_wrap set by writing the last column via
/// `write_char_wrap` (`write_char` alone does not defer the wrap).
fn grid_with_pending_wrap(rows: u16, cols: u16) -> Grid {
    let mut grid = Grid::new(rows, cols);
    grid.set_cursor(0, 0);
    // Fill first row to the last column to trigger pending_wrap.
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

/// Each operation either consumes the parked wrap (xterm `ResetWrap`) or
/// keeps it. IL/DL reset it only once their margin tests pass: xterm's
/// `InsertLine`/`DeleteLine` return before `ResetWrap` when the cursor row is
/// outside the scroll region or its column outside the DECLRMM margins. SU/SD
/// keep it (`xtermScroll` saves and restores `do_wrap`, `RevScroll` never
/// touches it), as do HT and CBT (`TabToNextStop`/`TabToPrevStop`). LF always
/// resets it (`CursorDown`), moving or scrolling, or the next glyph would
/// scroll a second time.
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

// wrap_serial: the emulator wrap fact

/// `wrap_serial` bumps once per resolved autowrap and never on a hard newline:
/// a parked `pending_wrap` is not yet a wrap, the resolving glyph is one wrap,
/// and CR+LF stays invisible to it.
#[test]
fn wrap_serial_counts_resolved_wraps_and_ignores_hard_newlines() {
    let mut grid = grid_with_pending_wrap(3, 5);
    assert_eq!(
        grid.wrap_serial(),
        0,
        "a parked deferred wrap is not yet a wrap"
    );

    // The next printable resolves the deferred wrap: exactly one bump.
    grid.write_char_wrap('f');
    assert_eq!(grid.wrap_serial(), 1, "one wrapped line, one bump");
    assert_eq!(grid.cursor_row(), 1, "the resolve advanced the line");

    // Ordinary typing inside the line never bumps.
    grid.write_char_wrap('g');
    assert_eq!(grid.wrap_serial(), 1, "mid-line typing is not a wrap");

    // A hard newline (CR + LF) moves the caret the same way on glass but
    // must stay invisible to the wrap fact.
    grid.carriage_return();
    grid.line_feed();
    assert_eq!(grid.wrap_serial(), 1, "a hard newline is not a wrap");

    // A second full line resolves a second wrap: exactly one more bump.
    for col in 0..5 {
        grid.write_char_wrap((b'A' + col as u8) as char);
    }
    assert_eq!(grid.wrap_serial(), 1, "the second wrap is still parked");
    grid.write_char_wrap('h');
    assert_eq!(grid.wrap_serial(), 2, "second wrapped line, second bump");
}
