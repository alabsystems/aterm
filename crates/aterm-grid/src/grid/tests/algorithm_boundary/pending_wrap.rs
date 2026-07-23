// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending wrap conformance tests.
//!
//! Verifies that operations which should clear the `pending_wrap` flag
//! (xterm's `wrapnext` / `do_wrap`) actually do so. Per xterm behavior,
//! insert/delete character/line operations and erase-character (ECH) must
//! cancel deferred wrap state.
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
// Regression: erase operations already clear pending_wrap
// ========================================================================

/// ED mode 2 (Erase in Display) preserves pending_wrap — xterm clears cells but
/// never resets the deferred wrap; a later glyph still wraps.
#[test]
fn erase_screen_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_screen();
    assert!(grid.pending_wrap(), "ED mode 2 must preserve pending_wrap");
}

/// EL mode 2 (Erase in Line) preserves pending_wrap (xterm).
#[test]
fn erase_line_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_line();
    assert!(grid.pending_wrap(), "EL mode 2 must preserve pending_wrap");
}

/// EL mode 0 (erase to end) PRESERVES pending_wrap and the parked last cell.
///
/// xterm.js encodes pending-wrap as x==cols, so EL-0 erases the empty range
/// [cols, cols) — it touches neither the last glyph nor the wrap state. A later
/// glyph still wraps to the next row (conformance: el-pending-wrap).
#[test]
fn erase_to_end_of_line_keeps_pending_wrap_and_last_cell() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_to_end_of_line();
    assert!(grid.pending_wrap(), "EL mode 0 must preserve pending_wrap");
    assert_eq!(
        grid.cell(0, 4).unwrap().char(),
        'E',
        "EL mode 0 must not erase the parked last cell"
    );
}

/// EL mode 1 (erase from start) preserves pending_wrap (xterm).
#[test]
fn erase_from_start_of_line_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_from_start_of_line();
    assert!(grid.pending_wrap(), "EL mode 1 must preserve pending_wrap");
}

/// ED mode 0 (erase to end of screen) PRESERVES pending_wrap and the parked cell.
///
/// Like EL-0, the cursor is logically past the last cell, so the current row's
/// erase-to-end clears nothing and the wrap survives; only rows below the cursor
/// are cleared (conformance: ed-pending-wrap).
#[test]
fn erase_to_end_of_screen_keeps_pending_wrap_and_last_cell() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_to_end_of_screen();
    assert!(grid.pending_wrap(), "ED mode 0 must preserve pending_wrap");
    assert_eq!(
        grid.cell(0, 4).unwrap().char(),
        'E',
        "ED mode 0 must not erase the parked last cell"
    );
}

/// ED mode 1 (erase from start of screen) preserves pending_wrap (xterm).
#[test]
fn erase_from_start_of_screen_keeps_pending_wrap() {
    let mut grid = grid_with_pending_wrap(3, 5);
    grid.erase_from_start_of_screen();
    assert!(grid.pending_wrap(), "ED mode 1 must preserve pending_wrap");
}

/// DECERA (erase rectangular area) preserves pending_wrap (xterm).
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
