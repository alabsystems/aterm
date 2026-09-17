// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending wrap conformance tests.
//!
//! Verifies that operations which should clear the `pending_wrap` flag
//! (xterm's `do_wrap`) actually do so, and that the ones xterm leaves alone keep
//! it. Per xterm (`util.c` / `charproc.c`), these cancel the deferred wrap:
//! insert/delete character/line (ICH/DCH/IL/DL), erase-character (ECH), the
//! ED 0/1/2 and EL 0/1/2 erases (`ClearRight`, `ClearInLine2` and
//! `ClearScreen` all call `ResetWrap`), the selective DECSEL/DECSED erases
//! (DECSEL 0 always; DECSED 0 except at the origin, reachable only on a line one
//! character wide, where xterm's `do_erase_display` reduces it to DECSED 2;
//! modes 1/2 and that origin case unless every cell of the span is protected,
//! because `ClearInLine2` returns before `ResetWrap` when its span holds no
//! unprotected cell), DECALN, and
//! entering the alternate screen with CSI ?1049 h (its `ClearScreen` runs
//! last). Scrolls (SU/SD), TAB, ED 3 and the rectangle ops keep it.
//!
//! Part of #5351 (deferred wrapping conformance).

use super::super::*;

/// Helper: create a grid and set pending_wrap by writing to the last column.
///
/// Uses `write_char_wrap` (deferred autowrap) so the cursor advances through
/// each column and sets `pending_wrap = true` when the last column is written.
/// `write_char` alone does not trigger deferred wrap.
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

// ========================================================================
// ICH / DCH — Insert / Delete Characters
// ========================================================================

/// ICH (Insert Characters) must clear pending_wrap.
///
/// xterm: `InsertChar()` calls `ResetWrap(screen)` before inserting.
#[test]
fn insert_chars_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);

    grid.insert_chars(1);

    assert!(
        !grid.pending_wrap(),
        "ICH must clear pending_wrap (xterm: ResetWrap in InsertChar)"
    );
}

/// DCH (Delete Characters) must clear pending_wrap.
///
/// xterm: `DeleteChar()` calls `ResetWrap(screen)` before deleting.
#[test]
fn delete_chars_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);

    grid.delete_chars(1);

    assert!(
        !grid.pending_wrap(),
        "DCH must clear pending_wrap (xterm: ResetWrap in DeleteChar)"
    );
}

// ========================================================================
// IL / DL — Insert / Delete Lines
// ========================================================================

/// IL (Insert Lines) must clear pending_wrap.
///
/// xterm: `InsertLine()` calls `ResetWrap(screen)`.
#[test]
fn insert_lines_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);

    grid.insert_lines(1);

    assert!(
        !grid.pending_wrap(),
        "IL must clear pending_wrap (xterm: ResetWrap in InsertLine)"
    );
}

/// DL (Delete Lines) must clear pending_wrap.
///
/// xterm: `DeleteLine()` calls `ResetWrap(screen)`.
#[test]
fn delete_lines_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);

    grid.delete_lines(1);

    assert!(
        !grid.pending_wrap(),
        "DL must clear pending_wrap (xterm: ResetWrap in DeleteLine)"
    );
}

// ========================================================================
// ECH — Erase Characters
// ========================================================================

/// ECH (Erase Characters) must clear pending_wrap.
///
/// xterm: `ClearRight()` path through ECH clears `wrapnext`.
#[test]
fn erase_chars_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);

    grid.erase_chars(1);

    assert!(!grid.pending_wrap(), "ECH must clear pending_wrap");
}

// ========================================================================
// DECALN — Screen Alignment Pattern
// ========================================================================

/// DECALN must clear pending_wrap.
///
/// xterm: screen alignment pattern resets cursor to home and clears wrapnext.
#[test]
fn screen_alignment_pattern_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);

    grid.screen_alignment_pattern();

    assert!(!grid.pending_wrap(), "DECALN must clear pending_wrap");
    assert_eq!(grid.cursor_row(), 0, "cursor at home row");
    assert_eq!(grid.cursor_col(), 0, "cursor at home col");
}

// ========================================================================
// ED / EL — erases clear pending_wrap (xterm util.c ResetWrap)
// ========================================================================
//
// These six used to assert the xterm.js rule (pending wrap stored as
// x == cols, so an erase keeps both the wrap and the parked glyph). Real xterm
// resets the wrap on every one of them.

/// ED mode 2 (Erase in Display) clears pending_wrap.
///
/// xterm: `do_erase_display` case 2 -> `ClearScreen`, which calls `ResetWrap`.
#[test]
fn erase_screen_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_screen();
    assert!(!grid.pending_wrap(), "ED mode 2 must clear pending_wrap");
}

/// EL mode 2 (Erase in Line) clears pending_wrap.
///
/// xterm: `do_erase_line` case 2 -> `ClearLine` -> `ClearInLine2` `ResetWrap`.
#[test]
fn erase_line_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_line();
    assert!(!grid.pending_wrap(), "EL mode 2 must clear pending_wrap");
}

/// EL mode 0 (erase to end) clears pending_wrap AND the parked last cell.
///
/// xterm: `do_erase_line` case 0 -> `ClearRight(xw, -1)`, which clears from
/// `cur_col` inclusive — the parked last column while a wrap is pending — and
/// ends with `ResetWrap`.
#[test]
fn erase_to_end_of_line_clears_pending_wrap_and_last_cell() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_to_end_of_line();
    assert!(!grid.pending_wrap(), "EL mode 0 must clear pending_wrap");
    assert_eq!(
        grid.cell(0, 4).unwrap().char(),
        ' ',
        "EL mode 0 must erase the parked last cell"
    );
}

/// EL mode 1 (erase from start) clears pending_wrap.
///
/// xterm: `do_erase_line` case 1 -> `ClearLeft` -> `ClearInLine2` `ResetWrap`.
#[test]
fn erase_from_start_of_line_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_from_start_of_line();
    assert!(!grid.pending_wrap(), "EL mode 1 must clear pending_wrap");
}

/// ED mode 0 (erase to end of screen) clears pending_wrap AND the parked cell.
///
/// xterm: `do_erase_display` case 0 -> `ClearBelow` -> `ClearRight(xw, -1)`,
/// the same inclusive clear and `ResetWrap` as EL 0, then the rows below.
#[test]
fn erase_to_end_of_screen_clears_pending_wrap_and_last_cell() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_to_end_of_screen();
    assert!(!grid.pending_wrap(), "ED mode 0 must clear pending_wrap");
    assert_eq!(
        grid.cell(0, 4).unwrap().char(),
        ' ',
        "ED mode 0 must erase the parked last cell"
    );
}

/// ED mode 1 (erase from start of screen) clears pending_wrap.
///
/// xterm: `do_erase_display` case 1 -> `ClearAbove` -> `ClearLeft` ->
/// `ClearInLine2` `ResetWrap` (or, at bottom-right, `ClearScreen`).
#[test]
fn erase_from_start_of_screen_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_from_start_of_screen();
    assert!(!grid.pending_wrap(), "ED mode 1 must clear pending_wrap");
}

// ========================================================================
// Rectangle ops PRESERVE pending_wrap
// ========================================================================

/// DECERA (erase rectangular area) preserves pending_wrap.
///
/// xterm: `CASE_DECERA` -> screen.c `ScrnFillRectangle`; screen.c never calls
/// `ResetWrap` or writes `do_wrap`.
#[test]
fn erase_rect_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_rect(0, 0, 2, 4);
    assert!(grid.pending_wrap(), "DECERA must preserve pending_wrap");
}

// ========================================================================
// Scroll operations PRESERVE pending_wrap
// ========================================================================

/// CSI S (Scroll Up) must PRESERVE pending_wrap.
///
/// xterm: CASE_SU -> `xtermScroll()`, which explicitly saves and restores
/// `screen->do_wrap` around the scroll (util.c `save_wrap`).
#[test]
fn scroll_region_up_preserves_pending_wrap() {
    let mut grid = grid_with_pending_wrap(5, 5);
    grid.scroll_region_up(1);
    assert!(
        grid.pending_wrap(),
        "CSI S (scroll region up) must preserve pending_wrap (xterm xtermScroll save_wrap)"
    );
}

/// CSI T (Scroll Down) must PRESERVE pending_wrap.
///
/// xterm: CASE_SD -> `RevScroll()`, which never touches `screen->do_wrap`.
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

/// LF always clears pending_wrap — it is a cursor-down (xterm CursorDown ends
/// with ResetWrap), whether it moves the cursor or scrolls at the region bottom.
/// (Only the explicit SU/SD CSI ops preserve it, via xtermScroll save_wrap; those
/// don't go through line_feed.) A scrolling LF that kept the flag would make the
/// next glyph trigger a second scroll — verified against xterm.js (cursorX 3, not
/// 4, after `\x1b[3;1Habcd\n`).
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

// ========================================================================
// Tab / Back Tab — clear pending_wrap
// ========================================================================

/// HT (Tab) must PRESERVE pending_wrap.
///
/// xterm: `TabToNextStop()` (tabs.c) only calls `set_cur_col` and never
/// touches `screen->do_wrap` — a TAB issued while wrap is pending leaves
/// the cursor at the margin with the wrap still pending, so the next
/// printable wraps instead of overprinting the last column.
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

/// CBT (Back Tab) must clear pending_wrap.
///
/// xterm: Back tab repositions the cursor, cancelling the deferred wrap state.
#[test]
fn back_tab_clears_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 20);

    grid.back_tab();

    assert!(
        !grid.pending_wrap(),
        "CBT (back tab) must clear pending_wrap"
    );
}

// ========================================================================
// wrap_serial — the emulator wrap fact (kitty-motion §4.1)
// ========================================================================

/// `wrap_serial` bumps exactly once per RESOLVED autowrap line and never on
/// a hard newline.
///
/// The serial is the host-facing wrap FACT: a parked `pending_wrap` is not
/// yet a wrap (xterm defers the advance until the next printable), the
/// resolving glyph is exactly one wrap, and CR+LF — the same caret shape on
/// glass — must stay invisible to it, or the pet's fold would eat honest
/// Enter presses.
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
