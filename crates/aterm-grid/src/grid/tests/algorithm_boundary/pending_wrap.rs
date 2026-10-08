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

// IL / DL

/// IL clears pending_wrap (xterm `InsertLine()` calls `ResetWrap`).
#[test]
fn insert_lines_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);

    grid.insert_lines(1);

    assert!(
        !grid.pending_wrap(),
        "IL must clear pending_wrap (xterm: ResetWrap in InsertLine)"
    );
}

/// An IL/DL the cursor position rejects keeps the parked wrap.
///
/// xterm's `InsertLine` and `DeleteLine` (util.c) test the margins and return
/// before `ResetWrap(screen)`:
///
/// ```c
/// if (!ScrnIsRowInMargins(screen, screen->cur_row)
///     || screen->cur_col < left || screen->cur_col > right)
///     return;                       /* <-- before ResetWrap */
/// ...
/// ResetWrap(screen);
/// ```
///
/// Each refusal sits beside the in-range case that still clears.
#[test]
fn insert_lines_outside_the_scroll_region_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    // The cursor is parked on row 0; put the scroll region strictly below it.
    grid.set_scroll_region(1, 4);
    assert!(
        grid.pending_wrap(),
        "precondition: setting the region must not disturb the parked wrap"
    );

    grid.insert_lines(1);

    assert!(
        grid.pending_wrap(),
        "an IL the region test rejects must leave the wrap armed (xterm returns before ResetWrap)"
    );

    // Positive control: the same IL with the cursor inside the region resets.
    grid.set_scroll_region(0, 4);
    grid.insert_lines(1);
    assert!(
        !grid.pending_wrap(),
        "an IL that actually inserts must still reset the wrap"
    );
}

/// DL outside the scroll region keeps the wrap — see the IL twin above.
#[test]
fn delete_lines_outside_the_scroll_region_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    grid.set_scroll_region(1, 4);

    grid.delete_lines(1);

    assert!(
        grid.pending_wrap(),
        "a DL the region test rejects must leave the wrap armed"
    );

    // Positive control.
    grid.set_scroll_region(0, 4);
    grid.delete_lines(1);
    assert!(
        !grid.pending_wrap(),
        "a DL that actually deletes must still reset the wrap"
    );
}

/// IL outside the DECLRMM horizontal margins keeps the wrap (xterm's column
/// test is in the same early return).
#[test]
fn insert_lines_outside_the_horizontal_margins_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    // The wrap parks the cursor on the last column; these margins exclude it.
    grid.set_horizontal_margins(0, 2);
    assert_eq!(grid.cursor_col(), 4, "precondition: cursor outside [0, 2]");

    grid.insert_lines_margined(1, true);

    assert!(
        grid.pending_wrap(),
        "an IL the column-margin test rejects must leave the wrap armed"
    );

    // Positive control: margins containing the parked column reset it.
    grid.set_horizontal_margins(2, 4);
    grid.insert_lines_margined(1, true);
    assert!(
        !grid.pending_wrap(),
        "an IL inside the margins must still reset the wrap"
    );
}

/// DL outside the DECLRMM horizontal margins keeps the wrap — see the IL twin.
#[test]
fn delete_lines_outside_the_horizontal_margins_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    grid.set_horizontal_margins(0, 2);

    grid.delete_lines_margined(1, true);

    assert!(
        grid.pending_wrap(),
        "a DL the column-margin test rejects must leave the wrap armed"
    );

    // Positive control.
    grid.set_horizontal_margins(2, 4);
    grid.delete_lines_margined(1, true);
    assert!(
        !grid.pending_wrap(),
        "a DL inside the margins must still reset the wrap"
    );
}

/// DL clears pending_wrap (xterm `DeleteLine()` calls `ResetWrap`).
#[test]
fn delete_lines_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);

    grid.delete_lines(1);

    assert!(
        !grid.pending_wrap(),
        "DL must clear pending_wrap (xterm: ResetWrap in DeleteLine)"
    );
}

// Scroll operations keep pending_wrap

/// CSI S (SU) keeps pending_wrap: xterm's `xtermScroll()` saves and restores
/// `do_wrap`.
#[test]
fn scroll_region_up_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    grid.scroll_region_up(1);
    assert!(
        grid.pending_wrap(),
        "CSI S (scroll region up) must preserve pending_wrap (xterm xtermScroll save_wrap)"
    );
}

/// CSI T (SD) keeps pending_wrap: xterm's `RevScroll()` never touches `do_wrap`.
#[test]
fn scroll_region_down_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    grid.scroll_region_down(1);
    assert!(
        grid.pending_wrap(),
        "CSI T (scroll region down) must preserve pending_wrap (xterm RevScroll)"
    );
}

/// Full-screen scroll_up preserves pending_wrap (same xtermScroll contract).
#[test]
fn scroll_up_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    grid.scroll_up(1);
    assert!(
        grid.pending_wrap(),
        "scroll_up must preserve pending_wrap (xterm xtermScroll save_wrap)"
    );
}

/// LF always clears pending_wrap (xterm `CursorDown` ends with `ResetWrap`),
/// whether it moves the cursor or scrolls at the region bottom; otherwise the
/// next glyph would scroll a second time.
#[test]
fn line_feed_always_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.set_scroll_region(0, 2);
    // Cursor on row 0, below-region-bottom move branch.
    grid.line_feed();
    assert!(!grid.pending_wrap(), "moving LF must clear pending_wrap");

    let mut grid = grid_with_pending_wrap(1, 5); // cursor at region bottom
    grid.line_feed();
    assert!(
        !grid.pending_wrap(),
        "scrolling LF must clear pending_wrap too"
    );
}

/// DECSC/DECRC round-trip the deferred wrap flag.
#[test]
fn save_restore_cursor_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    let saved_cursor = grid.cursor();

    grid.save_cursor();
    grid.carriage_return();

    assert!(
        !grid.pending_wrap(),
        "carriage return must clear pending_wrap before restore"
    );

    grid.restore_cursor();

    assert_eq!(
        grid.cursor(),
        saved_cursor,
        "DECRC must restore cursor position"
    );
    assert!(
        grid.pending_wrap(),
        "DECRC must restore the deferred wrap state captured by DECSC"
    );
}

// Tab / back tab keep pending_wrap

/// HT keeps pending_wrap: xterm's `TabToNextStop()` never touches `do_wrap`, so
/// the next printable wraps instead of overprinting the last column.
#[test]
fn tab_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 20);

    grid.tab();

    assert!(
        grid.pending_wrap(),
        "HT (tab) must preserve pending_wrap (xterm: TabToNextStop never touches do_wrap)"
    );
    assert_eq!(grid.cursor_col(), 19, "cursor stays at the last column");
}

/// CBT keeps pending_wrap like HT: xterm's `TabToPrevStop` only assigns
/// `cur_col` and never calls `ResetWrap`.
#[test]
fn back_tab_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 20);

    grid.back_tab();

    assert!(
        grid.pending_wrap(),
        "CBT must preserve pending_wrap (xterm: TabToPrevStop never touches do_wrap)"
    );
    assert_eq!(
        grid.cursor_col(),
        16,
        "the cursor still moves to the previous tab stop"
    );
}

/// The margin-aware back tab preserves it too — same function, same reason.
/// (Origin mode on, so the left-margin branch is the one exercised.)
#[test]
fn back_tab_margin_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 20);
    grid.set_horizontal_margins(4, 19);

    grid.back_tab_margin(true, true);

    assert!(
        grid.pending_wrap(),
        "margin-aware CBT must preserve pending_wrap as well"
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
