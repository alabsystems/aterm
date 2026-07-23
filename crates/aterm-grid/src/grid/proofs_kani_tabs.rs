// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for tab stop operations (cursor_ops.rs tab functions).
//!
//! Verifies the documented REQUIRES/ENSURES contracts for tab stop
//! functions in `Grid`. Uses `Grid::kani_mock` with fixed 4×8 dimensions.
//!
//! Part of #376 (terminal module proof coverage gap).

use super::*;
use crate::Cell;

// =============================================================================
// tab / back_tab — cursor movement through tab stops
// =============================================================================

/// `tab` advances cursor column to next tab stop (or max_col).
///
/// ENSURES: cursor.col >= old_col
/// ENSURES: cursor.col <= max_col_for_row(cursor.row)
#[kani::proof]
fn tab_advances_or_stays() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    let old_col = grid.cursor().col;

    grid.tab();

    kani::assert(grid.cursor().col >= old_col, "tab: cursor moved backward");
    kani::assert(
        grid.cursor().col <= grid.storage.max_col_for_row(grid.cursor().row),
        "tab: cursor exceeded max column",
    );
}

/// `back_tab` moves cursor to previous tab stop (or column 0).
///
/// ENSURES: cursor.col <= old_col
#[kani::proof]
fn back_tab_retreats_or_stays() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    let old_col = grid.cursor().col;

    grid.back_tab();

    kani::assert(
        grid.cursor().col <= old_col,
        "back_tab: cursor moved forward",
    );
}

/// `tab` from column 0 with default tab stops lands at column 7 (max_col)
/// on an 8-column grid, because the first tab stop at col 8 is at the boundary.
///
/// On KANI_MOCK_COLS = 8, max_col = 7. Tab stops at multiples of 8 where c > 0,
/// so the only stop is at col 8 — which is at max_col (since default tabs are
/// `c > 0 && c % 8 == 0`). So tab from col 0 should go to col 7 (no stop before max).
///
/// Wait — default_tab_stops has a stop at col 8 only if cols > 8. With cols=8,
/// tab_stops has indices 0..7. The tab at col 8 is the 9th element (index 8)
/// which doesn't exist. So tab from col 0 on 8-col grid goes to col 7 (max_col).
// INTENTIONALLY_CONCRETE: tests zero/empty edge case (col=0, 8-col grid specific behavior)
///
/// Also verifies that tab from any column without intervening tab stops
/// advances to max_col, using symbolic starting column in a cleared-stops grid.
#[kani::proof]
fn tab_from_zero_default_stops() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Default tab stops: col % 8 == 0 && col > 0. For 8 cols (indices 0..7),
    // no column satisfies this (col 8 is out of bounds).
    grid.set_cursor(0, 0);
    grid.tab();

    // Should land at max_col since no tab stop exists in 0..7
    kani::assert(
        grid.cursor().col == Grid::KANI_MOCK_COLS - 1,
        "tab from 0 with default stops: should go to max_col",
    );

    // Additional: clear all tab stops and verify tab from symbolic column goes to max_col
    grid.clear_all_tab_stops();
    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    grid.tab();

    kani::assert(
        grid.cursor().col == grid.storage.max_col_for_row(grid.cursor().row),
        "tab with no stops from symbolic column must go to max_col",
    );
}

// =============================================================================
// tab_n / back_tab_n — multi-stop movement
// =============================================================================

/// `tab_n` advances through n tab stops monotonically.
///
/// ENSURES: cursor.col >= old_col
/// ENSURES: cursor.col <= max_col_for_row(cursor.row)
#[kani::proof]
fn tab_n_monotonic() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    let old_col = grid.cursor().col;

    let n: u16 = kani::any();
    kani::assume(n <= 4); // Bound to keep verification tractable
    grid.tab_n(n);

    kani::assert(grid.cursor().col >= old_col, "tab_n: cursor moved backward");
    kani::assert(
        grid.cursor().col <= grid.storage.max_col_for_row(grid.cursor().row),
        "tab_n: cursor exceeded max column",
    );
}

/// `back_tab_n` retreats through n tab stops monotonically.
///
/// ENSURES: cursor.col <= old_col
#[kani::proof]
fn back_tab_n_monotonic() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    let old_col = grid.cursor().col;

    let n: u16 = kani::any();
    kani::assume(n <= 4); // Bound to keep verification tractable
    grid.back_tab_n(n);

    kani::assert(
        grid.cursor().col <= old_col,
        "back_tab_n: cursor moved forward",
    );
}

// =============================================================================
// set_tab_stop / clear_tab_stop / clear_all_tab_stops
// =============================================================================

/// `set_tab_stop` marks current cursor column as a tab stop.
///
/// ENSURES: cursor.col in range implies tab_stops[cursor.col] == true
#[kani::proof]
fn set_tab_stop_marks_column() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col);

    grid.set_tab_stop();

    kani::assert(
        grid.storage.is_tab_stop(grid.cursor().col),
        "set_tab_stop: column not marked as tab stop",
    );
}

/// `clear_tab_stop` clears the tab stop at current cursor column.
///
/// ENSURES: cursor.col in range implies tab_stops[cursor.col] == false
#[kani::proof]
fn clear_tab_stop_clears_column() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col);

    // First set, then clear
    grid.set_tab_stop();
    grid.clear_tab_stop();

    kani::assert(
        !grid.storage.is_tab_stop(grid.cursor().col),
        "clear_tab_stop: column still marked as tab stop",
    );
}

/// `clear_all_tab_stops` clears every tab stop.
///
/// ENSURES: all tab_stops are false
#[kani::proof]
fn clear_all_tab_stops_clears_all() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set some tab stops first
    grid.set_cursor(0, 2);
    grid.set_tab_stop();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();

    grid.clear_all_tab_stops();

    // Check every column
    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    kani::assert(
        !grid.storage.is_tab_stop(col),
        "clear_all_tab_stops: some tab stop still set",
    );
}

/// `set_tab_stop` then `clear_tab_stop` roundtrips to cleared state.
#[kani::proof]
fn set_clear_tab_stop_roundtrip() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col);

    let _was_set = grid.storage.is_tab_stop(col);
    grid.set_tab_stop();
    kani::assert(grid.storage.is_tab_stop(col), "set_tab_stop didn't set");
    grid.clear_tab_stop();
    kani::assert(
        !grid.storage.is_tab_stop(col),
        "clear_tab_stop didn't clear",
    );
}

// =============================================================================
// is_tab_stop — bounds safety
// =============================================================================

/// `is_tab_stop` returns false for out-of-bounds columns.
///
/// ENSURES: col >= cols implies result == false
#[kani::proof]
fn is_tab_stop_oob_returns_false() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let grid = Grid::kani_mock(&mut cells);

    let col: u16 = kani::any();
    kani::assume(col >= Grid::KANI_MOCK_COLS);

    kani::assert(
        !grid.storage.is_tab_stop(col),
        "is_tab_stop: returned true for out-of-bounds column",
    );
}

// =============================================================================
// reset_tab_stops — restores default pattern
// =============================================================================

/// `reset_tab_stops` restores the tab stop vector to the correct length
/// regardless of how many symbolic tab stops were set/cleared beforehand.
///
/// ENSURES: tab_stops.len() == cols
#[kani::proof]
fn reset_tab_stops_restores_length() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Modify tab stops at a symbolic column before resetting
    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col);
    grid.set_tab_stop();

    // Also clear at another symbolic column
    let col2: u16 = kani::any();
    kani::assume(col2 < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col2);
    grid.clear_tab_stop();

    grid.reset_tab_stops();

    kani::assert(
        grid.storage.tab_stops.len() == Grid::KANI_MOCK_COLS as usize,
        "reset_tab_stops: wrong tab_stops length after symbolic set/clear",
    );

    // Verify default tab stop pattern: col % 8 == 0 && col > 0
    // (On 8-col grid, no default stop exists in range 0..7 since col 8 is OOB)
    kani::assert(
        !grid.storage.is_tab_stop(0),
        "reset_tab_stops: col 0 should not be a tab stop",
    );
}
