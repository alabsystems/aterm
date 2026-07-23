// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for scroll operations (scroll.rs).
//!
//! Verifies the documented REQUIRES/ENSURES contracts for scroll
//! functions in `Grid`. Uses `Grid::kani_mock` and `Grid::kani_stub`
//! with bounded dimensions to keep verification tractable.
//!
//! Part of #376 (terminal module proof coverage gap).

use super::*;
use crate::Cell;

// =============================================================================
// scroll_display — display offset bounds
// =============================================================================

/// `scroll_display` keeps display_offset within scrollback bounds.
///
/// ENSURES: display_offset <= scrollback_lines()
#[kani::proof]
fn scroll_display_offset_bounded() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let delta: i32 = kani::any();
    // Bound delta to avoid i32 overflow in saturating_add
    kani::assume(delta >= -100 && delta <= 100);

    grid.scroll_display(delta);

    kani::assert(
        grid.display_offset() <= grid.scrollback_lines(),
        "scroll_display: display_offset exceeds scrollback",
    );
}

/// `scroll_display` with positive delta increases display_offset (when scrollback exists).
/// With no scrollback (mock grid), display_offset stays 0.
#[kani::proof]
fn scroll_display_positive_no_scrollback() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Mock grid has no scrollback, so scrollback_lines() == 0
    let delta: i32 = kani::any();
    kani::assume(delta >= 0 && delta <= 100);

    grid.scroll_display(delta);

    // With no scrollback, offset stays 0
    kani::assert(
        grid.display_offset() == 0,
        "scroll_display: offset nonzero with no scrollback",
    );
}

// =============================================================================
// scroll_to_top / scroll_to_bottom
// =============================================================================

/// `scroll_to_top` sets display_offset to scrollback_lines() regardless of prior scroll state.
///
/// ENSURES: display_offset == scrollback_lines()
#[kani::proof]
fn scroll_to_top_sets_max_offset() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Apply a symbolic scroll delta first so the display_offset is not trivially 0
    let pre_delta: i32 = kani::any();
    kani::assume(pre_delta >= -50 && pre_delta <= 50);
    grid.scroll_display(pre_delta);

    grid.scroll_to_top();

    kani::assert(
        grid.display_offset() == grid.scrollback_lines(),
        "scroll_to_top: display_offset != scrollback_lines after symbolic pre-scroll",
    );
}

/// `scroll_to_bottom` sets display_offset to 0 regardless of prior scroll state.
///
/// ENSURES: display_offset == 0
#[kani::proof]
fn scroll_to_bottom_sets_zero() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Scroll by a symbolic delta first so display_offset starts non-trivially
    let pre_delta: i32 = kani::any();
    kani::assume(pre_delta >= -50 && pre_delta <= 50);
    grid.scroll_display(pre_delta);

    grid.scroll_to_bottom();

    kani::assert(
        grid.display_offset() == 0,
        "scroll_to_bottom: display_offset != 0 after symbolic pre-scroll",
    );
}

/// `scroll_to_top` then `scroll_to_bottom` roundtrips to offset 0
/// regardless of the initial scroll state set by a symbolic delta.
#[kani::proof]
fn scroll_top_bottom_roundtrip() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Start from a symbolic scroll position
    let pre_delta: i32 = kani::any();
    kani::assume(pre_delta >= -50 && pre_delta <= 50);
    grid.scroll_display(pre_delta);

    grid.scroll_to_top();
    let top_offset = grid.display_offset();
    kani::assert(
        top_offset == grid.scrollback_lines(),
        "scroll_to_top must set offset to scrollback_lines",
    );

    grid.scroll_to_bottom();
    kani::assert(
        grid.display_offset() == 0,
        "scroll top/bottom roundtrip: offset not 0 after symbolic pre-scroll",
    );
}

// =============================================================================
// clamp_display_offset
// =============================================================================

/// `clamp_display_offset` ensures display_offset <= scrollback_lines().
///
/// ENSURES: display_offset <= scrollback_lines()
#[kani::proof]
fn clamp_display_offset_bounded() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Manually set an invalid display_offset to test clamping
    grid.storage.display_offset = kani::any();
    kani::assume(grid.storage.display_offset <= 100); // Bound for tractability

    grid.clamp_display_offset();

    kani::assert(
        grid.display_offset() <= grid.scrollback_lines(),
        "clamp_display_offset: offset still exceeds scrollback",
    );
}

// =============================================================================
// set_scroll_region / reset_scroll_region
// =============================================================================

/// `set_scroll_region` with valid params sets the region correctly.
/// With invalid params, resets to full screen.
#[kani::proof]
fn set_scroll_region_validates_bounds() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    let top: u16 = kani::any();
    let bottom: u16 = kani::any();

    grid.set_scroll_region(top, bottom);

    let region = grid.scroll_region();
    // Region is always valid: top <= bottom, bottom < visible_rows
    kani::assert(
        region.top <= region.bottom,
        "set_scroll_region: top > bottom",
    );
    kani::assert(
        region.bottom < grid.rows(),
        "set_scroll_region: bottom >= visible_rows",
    );

    // If the input was valid, the region should match
    if top < bottom && bottom < grid.rows() {
        kani::assert(
            region.top == top && region.bottom == bottom,
            "set_scroll_region: valid input not preserved",
        );
    } else {
        // Invalid input resets to full screen
        kani::assert(
            region.top == 0 && region.bottom == grid.rows() - 1,
            "set_scroll_region: invalid input didn't reset to full screen",
        );
    }
}

/// `reset_scroll_region` restores full screen from any symbolic region.
#[kani::proof]
fn reset_scroll_region_restores_full() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set a symbolic restricted region before resetting
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    grid.set_scroll_region(top, bottom);

    grid.reset_scroll_region();

    let region = grid.scroll_region();
    kani::assert(
        region.top == 0 && region.bottom == grid.rows() - 1,
        "reset_scroll_region: not full screen after symbolic set_scroll_region",
    );
}

// =============================================================================
// scroll_region_down — content shift within region
// =============================================================================

/// `scroll_region_down` with n=0 is a no-op (no damage) regardless of scroll region.
// INTENTIONALLY_CONCRETE: tests zero/empty edge case (n=0)
#[kani::proof]
fn scroll_region_down_zero_noop() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set a symbolic scroll region before testing n=0
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    grid.set_scroll_region(top, bottom);
    grid.clear_damage();

    grid.scroll_region_down(0);

    // No damage should be marked for n=0 regardless of region
    kani::assert(
        !grid.needs_full_redraw(),
        "scroll_region_down(0): marked damage for no-op with symbolic scroll region",
    );
}

/// `scroll_region_down` with n > 0 marks damage.
///
/// ENSURES: n > 0 implies damage.is_full()
#[kani::proof]
fn scroll_region_down_marks_damage() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);
    grid.clear_damage();

    let n: usize = kani::any();
    kani::assume(n >= 1 && n <= 4);

    grid.scroll_region_down(n);

    kani::assert(
        grid.needs_full_redraw(),
        "scroll_region_down: didn't mark damage for n > 0",
    );
}

// =============================================================================
// scroll_region_up — content shift within region
// =============================================================================

/// `scroll_region_up` with n=0 is a no-op regardless of scroll region.
// INTENTIONALLY_CONCRETE: tests zero/empty edge case (n=0)
#[kani::proof]
fn scroll_region_up_zero_noop() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set a symbolic scroll region before testing n=0
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    grid.set_scroll_region(top, bottom);
    grid.clear_damage();

    grid.scroll_region_up(0);

    kani::assert(
        !grid.needs_full_redraw(),
        "scroll_region_up(0): marked damage for no-op with symbolic scroll region",
    );
}

/// `scroll_region_up` with n > 0 marks damage.
///
/// ENSURES: n > 0 implies damage.is_full()
#[kani::proof]
fn scroll_region_up_marks_damage() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);
    grid.clear_damage();

    let n: usize = kani::any();
    kani::assume(n >= 1 && n <= 4);

    grid.scroll_region_up(n);

    kani::assert(
        grid.needs_full_redraw(),
        "scroll_region_up: didn't mark damage for n > 0",
    );
}

// =============================================================================
// scroll_up — ring buffer growth invariant
// =============================================================================

/// `scroll_up` maintains the ring buffer size invariant.
///
/// Uses kani_stub (not kani_mock) because scroll_up may add rows.
///
/// ENSURES: rows.len() <= visible_rows + max_scrollback
#[kani::proof]
fn scroll_up_ring_buffer_bounded() {
    let rows: u16 = kani::any();
    let cols: u16 = kani::any();
    kani::assume(rows >= 2 && rows <= KANI_MAX_ROWS);
    kani::assume(cols >= 2 && cols <= KANI_MAX_COLS);

    let mut grid = Grid::kani_stub(rows, cols);

    let n: usize = kani::any();
    kani::assume(n <= 4);

    grid.scroll_up(n);

    let max_size = (grid.rows() as usize) + grid.storage.max_scrollback;
    kani::assert(
        grid.storage.rows.len() <= max_size,
        "scroll_up: ring buffer exceeded capacity",
    );
}

/// `scroll_up(0)` is a no-op regardless of scroll region.
// INTENTIONALLY_CONCRETE: tests zero/empty edge case (n=0)
#[kani::proof]
fn scroll_up_zero_noop() {
    let mut cells = [[Cell::EMPTY; Grid::KANI_MOCK_COLS as usize]; Grid::KANI_MOCK_ROWS as usize];
    let mut grid = Grid::kani_mock(&mut cells);

    // Set a symbolic scroll region before testing n=0
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    grid.set_scroll_region(top, bottom);
    grid.clear_damage();

    grid.scroll_up(0);

    kani::assert(
        !grid.needs_full_redraw(),
        "scroll_up(0): marked damage for no-op with symbolic scroll region",
    );
}
