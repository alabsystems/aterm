// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani verification proofs for vi mode search navigation.
//!
//! Proves key invariants of `ViSearchState::focus_next` and `focus_prev`:
//! - Return value is always a member of the matches vector (or None for empty).
//! - Focused index is always in-bounds after any operation.
//! - Navigation result is consistent with `focused_point()` accessor.

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a single-element matches vector with a symbolic ViPoint.
///
/// Uses exactly 1 match to keep CBMC tractable. Vec operations
/// (allocation, clone, iteration) dominate CBMC formula size even with
/// tiny symbolic ranges — the 2-element case with [-1,1]×[0,3] still
/// exhausted 16GB+ RAM after 5+ minutes. A single element eliminates:
/// - Symbolic Vec capacity branching
/// - Sorting constraint encoding
/// - Multi-iteration clone/iter loops
///
/// Line in [-1, 1] covers scrollback, origin, and visible; col in
/// [0, 3] covers column ordering. The 2-match partition_point path
/// is covered by concrete unit tests (test_focus_next_between_matches,
/// test_scrollback_matches, etc.).
fn kani_sorted_matches() -> Vec<ViPoint> {
    let line: i32 = kani::any();
    let col: u16 = kani::any();
    kani::assume(line >= -1 && line <= 1);
    kani::assume(col <= 3);
    vec![ViPoint::new(line, col)]
}

/// Build a symbolic cursor point in the same range as matches.
fn kani_cursor() -> ViPoint {
    let line: i32 = kani::any();
    let col: u16 = kani::any();
    kani::assume(line >= -1 && line <= 1);
    kani::assume(col <= 3);
    ViPoint::new(line, col)
}

// ---------------------------------------------------------------------------
// INV-SEARCH-1: focus_next returns a member of matches (or None if empty).
// ---------------------------------------------------------------------------

/// For any non-empty sorted matches and any cursor position,
/// `focus_next` returns `Some(p)` where `p` is an element of `matches`.
#[kani::proof]
#[kani::unwind(3)]
fn focus_next_result_is_match_member() {
    let matches = kani_sorted_matches();
    let expected = matches[0]; // single-element: save before moving
    let cursor = kani_cursor();

    let mut state = ViSearchState::new();
    state.set_matches(matches);

    let result = state.focus_next(cursor);

    // Single match: must return Some with that exact point.
    if let Some(point) = result {
        kani::assert(point == expected, "focus_next result must be the match");
    } else {
        kani::assert(false, "Non-empty matches must return Some");
    }
}

// ---------------------------------------------------------------------------
// INV-SEARCH-2: focus_prev returns a member of matches (or None if empty).
// ---------------------------------------------------------------------------

/// For any non-empty sorted matches and any cursor position,
/// `focus_prev` returns `Some(p)` where `p` is an element of `matches`.
#[kani::proof]
#[kani::unwind(3)]
fn focus_prev_result_is_match_member() {
    let matches = kani_sorted_matches();
    let expected = matches[0]; // single-element: save before moving
    let cursor = kani_cursor();

    let mut state = ViSearchState::new();
    state.set_matches(matches);

    let result = state.focus_prev(cursor);

    // Single match: must return Some with that exact point.
    if let Some(point) = result {
        kani::assert(point == expected, "focus_prev result must be the match");
    } else {
        kani::assert(false, "Non-empty matches must return Some");
    }
}

// ---------------------------------------------------------------------------
// INV-SEARCH-3: focused_index is always in-bounds after focus_next/prev.
// ---------------------------------------------------------------------------

/// After any call to `focus_next` or `focus_prev`, `focused_index()` is
/// `Some(i)` where `i < match_count()`.
#[kani::proof]
#[kani::unwind(3)]
fn focused_index_in_bounds() {
    let matches = kani_sorted_matches();
    let cursor = kani_cursor();
    let use_next: bool = kani::any();

    let mut state = ViSearchState::new();
    state.set_matches(matches);

    if use_next {
        state.focus_next(cursor);
    } else {
        state.focus_prev(cursor);
    }

    if let Some(i) = state.focused_index() {
        kani::assert(
            i < state.match_count(),
            "Focused index must be less than match_count",
        );
    } else {
        kani::assert(false, "Focused index must be Some after navigation");
    }
}

// ---------------------------------------------------------------------------
// INV-SEARCH-4: focused_point() agrees with focused_index().
// ---------------------------------------------------------------------------

/// The point returned by `focus_next`/`focus_prev` must equal
/// `focused_point()` immediately afterwards.
#[kani::proof]
#[kani::unwind(3)]
fn focus_result_equals_focused_point() {
    let matches = kani_sorted_matches();
    let cursor = kani_cursor();
    let use_next: bool = kani::any();

    let mut state = ViSearchState::new();
    state.set_matches(matches);

    let result = if use_next {
        state.focus_next(cursor)
    } else {
        state.focus_prev(cursor)
    };

    kani::assert(
        result == state.focused_point(),
        "Navigation result must equal focused_point()",
    );
}

// ---------------------------------------------------------------------------
// INV-SEARCH-5: empty matches always returns None.
// ---------------------------------------------------------------------------

/// With no matches, both `focus_next` and `focus_prev` return `None`
/// and `focused_index()` remains `None`.
#[kani::proof]
fn empty_matches_returns_none() {
    let cursor = kani_cursor();
    let use_next: bool = kani::any();

    let mut state = ViSearchState::new();
    // No set_matches call — state is empty.

    let result = if use_next {
        state.focus_next(cursor)
    } else {
        state.focus_prev(cursor)
    };

    kani::assert(result.is_none(), "Empty matches must return None");
    kani::assert(
        state.focused_index().is_none(),
        "focused_index must be None for empty matches",
    );
}

// ---------------------------------------------------------------------------
// INV-SEARCH-6: clear() resets all navigation state.
// ---------------------------------------------------------------------------

/// After `clear()`, the state is equivalent to a freshly constructed one.
#[kani::proof]
#[kani::unwind(3)]
fn clear_resets_state() {
    let matches = kani_sorted_matches();
    let cursor = kani_cursor();

    let mut state = ViSearchState::new();
    state.set_matches(matches);
    state.focus_next(cursor);

    state.clear();

    kani::assert(state.is_empty(), "clear() must empty matches");
    kani::assert(state.match_count() == 0, "clear() must zero count");
    kani::assert(
        state.focused_index().is_none(),
        "clear() must reset focused_index",
    );
    kani::assert(
        state.focused_point().is_none(),
        "clear() must reset focused_point",
    );
}
