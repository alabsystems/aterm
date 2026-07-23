// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for cursor movement operations (cursor_ops.rs).
//!
//! Verifies the documented REQUIRES/ENSURES contracts for all cursor
//! movement functions in `Grid`. Uses `Grid::kani_mock` with fixed
//! 4×8 dimensions to avoid state explosion.
//!
//! Part of #376 (terminal module proof coverage gap).

use super::*;
use crate::Cell;

// =============================================================================
// Cursor positioning
// =============================================================================

/// `set_cursor` clamps row and col to valid bounds.
///
/// ENSURES: cursor.row < visible_rows
/// ENSURES: cursor.col <= max_col_for_row(cursor.row)
#[kani::proof]
fn set_cursor_clamps_to_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let row: u16 = kani::any();
    let col: u16 = kani::any();

    grid.set_cursor(row, col);

    kani::assert(
        grid.cursor().row < grid.rows(),
        "set_cursor: row out of bounds",
    );
    kani::assert(
        grid.cursor().col <= grid.storage.max_col_for_row(grid.cursor().row),
        "set_cursor: col out of bounds",
    );
}

/// `move_cursor_to` delegates to `set_cursor` with identical postconditions.
#[kani::proof]
fn move_cursor_to_clamps_to_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let row: u16 = kani::any();
    let col: u16 = kani::any();

    grid.move_cursor_to(row, col);

    kani::assert(
        grid.cursor().row < grid.rows(),
        "move_cursor_to: row out of bounds",
    );
    kani::assert(
        grid.cursor().col <= grid.storage.max_col_for_row(grid.cursor().row),
        "move_cursor_to: col out of bounds",
    );
}

// =============================================================================
// cursor_up / cursor_down — scroll region margin respect
// =============================================================================

/// `cursor_up` respects the scroll region top margin.
///
/// When the cursor starts within the scroll region, it stops at `scroll_region.top`.
/// When outside the scroll region, it stops at row 0.
///
/// ENSURES: cursor.row < visible_rows
/// ENSURES: (was in region) implies cursor.row >= scroll_region.top
#[kani::proof]
fn cursor_up_respects_scroll_region() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set up a symbolic scroll region within bounds
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    kani::assume(top < bottom && bottom < Grid::KANI_MOCK_ROWS);
    grid.set_scroll_region(top, bottom);

    // Place cursor within the scroll region
    let start_row: u16 = kani::any();
    let start_col: u16 = kani::any();
    kani::assume(start_row >= top && start_row <= bottom);
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(start_row, start_col);

    let n: u16 = kani::any();
    kani::assume(n <= Grid::KANI_MOCK_ROWS);

    grid.cursor_up(n);

    kani::assert(
        grid.cursor().row < grid.rows(),
        "cursor_up: row out of bounds",
    );
    // Cursor started in scroll region, so it must not go above top margin
    kani::assert(
        grid.cursor().row >= top,
        "cursor_up: went above scroll region top margin",
    );
}

/// `cursor_up` from outside scroll region stops at row 0.
#[kani::proof]
fn cursor_up_outside_region_stops_at_zero() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set scroll region to middle rows (1..2)
    grid.set_scroll_region(1, 2);

    // Place cursor above scroll region at row 0
    grid.set_cursor(0, 0);

    let n: u16 = kani::any();
    grid.cursor_up(n);

    // Should stay at 0 (already above scroll region)
    kani::assert(
        grid.cursor().row == 0,
        "cursor_up outside region: should stay at row 0",
    );
}

/// `cursor_down` respects the scroll region bottom margin.
///
/// When the cursor starts within the scroll region, it stops at `scroll_region.bottom`.
/// When outside the scroll region, it stops at the last row.
///
/// ENSURES: cursor.row < visible_rows
/// ENSURES: (was in region) implies cursor.row <= scroll_region.bottom
#[kani::proof]
fn cursor_down_respects_scroll_region() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set up a symbolic scroll region within bounds
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    kani::assume(top < bottom && bottom < Grid::KANI_MOCK_ROWS);
    grid.set_scroll_region(top, bottom);

    // Place cursor within the scroll region
    let start_row: u16 = kani::any();
    let start_col: u16 = kani::any();
    kani::assume(start_row >= top && start_row <= bottom);
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(start_row, start_col);

    let n: u16 = kani::any();
    kani::assume(n <= Grid::KANI_MOCK_ROWS);

    grid.cursor_down(n);

    kani::assert(
        grid.cursor().row < grid.rows(),
        "cursor_down: row out of bounds",
    );
    // Cursor started in scroll region, so it must not go below bottom margin
    kani::assert(
        grid.cursor().row <= bottom,
        "cursor_down: went below scroll region bottom margin",
    );
}

// =============================================================================
// cursor_forward / cursor_backward — column bounds
// =============================================================================

/// `cursor_forward` advances column without exceeding max_col.
///
/// ENSURES: cursor.col >= old_col
/// ENSURES: cursor.col <= max_col_for_row(cursor.row)
#[kani::proof]
fn cursor_forward_stays_in_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    let old_col = grid.cursor().col;

    let n: u16 = kani::any();
    grid.cursor_forward(n);

    kani::assert(
        grid.cursor().col >= old_col,
        "cursor_forward: col decreased",
    );
    kani::assert(
        grid.cursor().col <= grid.storage.max_col_for_row(grid.cursor().row),
        "cursor_forward: col exceeded max",
    );
}

/// `cursor_backward` moves column left without going below 0.
///
/// ENSURES: cursor.col <= old_col
#[kani::proof]
fn cursor_backward_stays_in_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_col: u16 = kani::any();
    kani::assume(start_col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, start_col);
    let old_col = grid.cursor().col;

    let n: u16 = kani::any();
    grid.cursor_backward(n);

    kani::assert(
        grid.cursor().col <= old_col,
        "cursor_backward: col increased",
    );
}

// =============================================================================
// carriage_return / backspace
// =============================================================================

/// `carriage_return` sets column to 0.
///
/// ENSURES: cursor.col == 0
#[kani::proof]
fn carriage_return_sets_col_zero() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col);

    grid.carriage_return();

    kani::assert(grid.cursor().col == 0, "carriage_return: col not zero");
}

/// `backspace` decrements column by 1 (saturating at 0).
///
/// ENSURES: cursor.col <= old_col
/// ENSURES: old_col > 0 implies cursor.col == old_col - 1
#[kani::proof]
fn backspace_decrements_col() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let col: u16 = kani::any();
    kani::assume(col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(0, col);
    let old_col = grid.cursor().col;

    grid.backspace();

    kani::assert(grid.cursor().col <= old_col, "backspace: col increased");
    if old_col > 0 {
        kani::assert(
            grid.cursor().col == old_col - 1,
            "backspace: col not decremented by 1",
        );
    } else {
        kani::assert(grid.cursor().col == 0, "backspace: col not saturated at 0");
    }
}

// =============================================================================
// line_feed / reverse_line_feed
// =============================================================================

/// `line_feed` keeps cursor row in bounds.
///
/// ENSURES: cursor.row < visible_rows
#[kani::proof]
fn line_feed_cursor_in_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Place cursor at a symbolic row
    let start_row: u16 = kani::any();
    kani::assume(start_row < Grid::KANI_MOCK_ROWS);
    grid.set_cursor(start_row, 0);

    grid.line_feed();

    kani::assert(
        grid.cursor().row < grid.rows(),
        "line_feed: row out of bounds",
    );
}

/// `line_feed` within scroll region: if cursor below bottom, stays at last row.
/// If at bottom, scrolls (row stays at bottom). If above bottom, moves down by 1.
#[kani::proof]
fn line_feed_behavior_in_scroll_region() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    kani::assume(top < bottom && bottom < Grid::KANI_MOCK_ROWS);
    grid.set_scroll_region(top, bottom);

    // Place cursor within scroll region but not at bottom
    let start_row: u16 = kani::any();
    kani::assume(start_row >= top && start_row < bottom);
    grid.set_cursor(start_row, 0);

    grid.line_feed();

    // Cursor should move down by 1 within the region
    kani::assert(
        grid.cursor().row == start_row + 1,
        "line_feed: expected cursor to advance by 1 within scroll region",
    );
}

/// `reverse_line_feed` keeps cursor row in bounds.
///
/// ENSURES: cursor.row < visible_rows
#[kani::proof]
fn reverse_line_feed_cursor_in_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let start_row: u16 = kani::any();
    kani::assume(start_row < Grid::KANI_MOCK_ROWS);
    grid.set_cursor(start_row, 0);

    grid.reverse_line_feed();

    kani::assert(
        grid.cursor().row < grid.rows(),
        "reverse_line_feed: row out of bounds",
    );
}

/// `reverse_line_feed` within scroll region: if above top, cursor moves up.
/// If at top, scrolls (row stays at top). If below top, moves up by 1.
#[kani::proof]
fn reverse_line_feed_behavior_in_scroll_region() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    kani::assume(top < bottom && bottom < Grid::KANI_MOCK_ROWS);
    grid.set_scroll_region(top, bottom);

    // Place cursor within scroll region but not at top
    let start_row: u16 = kani::any();
    kani::assume(start_row > top && start_row <= bottom);
    grid.set_cursor(start_row, 0);

    grid.reverse_line_feed();

    // Cursor should move up by 1 within the region
    kani::assert(
        grid.cursor().row == start_row - 1,
        "reverse_line_feed: expected cursor to retreat by 1 within scroll region",
    );
}

// =============================================================================
// save_cursor / restore_cursor
// =============================================================================

/// `save_cursor` marks saved state as valid and preserves position.
///
/// ENSURES: saved_cursor.valid == true
#[kani::proof]
fn save_cursor_marks_valid() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let row: u16 = kani::any();
    let col: u16 = kani::any();
    kani::assume(row < Grid::KANI_MOCK_ROWS && col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(row, col);

    let cursor_before = grid.cursor();
    grid.save_cursor();

    kani::assert(
        grid.storage.saved_cursor.valid,
        "save_cursor: saved_cursor not marked valid",
    );
    kani::assert(
        grid.storage.saved_cursor.cursor == cursor_before,
        "save_cursor: position not preserved",
    );
}

/// `restore_cursor` restores to in-bounds position after save.
///
/// ENSURES: cursor.row < visible_rows
#[kani::proof]
fn restore_cursor_clamps_to_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Save at a valid position
    let row: u16 = kani::any();
    let col: u16 = kani::any();
    kani::assume(row < Grid::KANI_MOCK_ROWS && col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(row, col);
    grid.save_cursor();

    // Move cursor elsewhere
    grid.set_cursor(0, 0);

    // Restore
    grid.restore_cursor();

    kani::assert(
        grid.cursor().row < grid.rows(),
        "restore_cursor: row out of bounds",
    );
    kani::assert(
        grid.cursor().col <= grid.storage.max_col_for_row(grid.cursor().row),
        "restore_cursor: col out of bounds",
    );
}

/// `save_cursor` then `restore_cursor` roundtrips cursor position.
#[kani::proof]
fn save_restore_cursor_roundtrip() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let row: u16 = kani::any();
    let col: u16 = kani::any();
    kani::assume(row < Grid::KANI_MOCK_ROWS && col < Grid::KANI_MOCK_COLS);
    grid.set_cursor(row, col);

    let saved_pos = grid.cursor();
    grid.save_cursor();
    grid.set_cursor(0, 0); // Move away
    grid.restore_cursor();

    kani::assert(
        grid.cursor() == saved_pos,
        "save/restore roundtrip: position changed",
    );
}

// =============================================================================
// set_cursor -> row_index validity chain (#6029 finding #5)
// =============================================================================

/// After `set_cursor`, `row_index(cursor.row)` returns a valid ring buffer index.
///
/// Proves the full chain: clamped cursor positions are always valid for
/// subsequent row access in live view (display_offset == 0).
///
/// ENSURES: row_index(cursor.row).is_some()
/// ENSURES: row_index(cursor.row) yields index < ring_buffer.len()
#[kani::proof]
fn set_cursor_row_index_valid_in_live_view() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let row: u16 = kani::any();
    let col: u16 = kani::any();

    grid.set_cursor(row, col);

    // In live view (display_offset == 0), row_index must succeed.
    let idx = grid.storage.row_index(grid.cursor().row);
    kani::assert(
        idx.is_some(),
        "set_cursor: row_index returned None in live view",
    );

    if let Some(idx) = idx {
        kani::assert(
            idx < grid.storage.rows.len(),
            "set_cursor: row_index out of ring buffer bounds",
        );
    }
}
