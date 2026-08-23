// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for the `TextSelection` state machine (INV-SEL-31 through INV-SEL-54,
//! plus INV-SEL-57/58 over `truncate_to_floor`).
//!
//! INV-SEL-55/56 over `intersects_absolute_band` are named by design §8 and are
//! DEFERRED, not forgotten: `scripts/verify-kani-proofs.sh` exits 3 without
//! trust-mc, so an unrunnable harness here would be a claim nothing checks.
//!
//! All proofs use symbolic inputs via `kani::any()` to explore the full bounded
//! input space. Strengthened from concrete-only versions (Part of #6850).

use super::*;

/// Helper: generate a bounded symbolic row.
fn any_row() -> i32 {
    let r: i32 = kani::any();
    kani::assume(r >= -100 && r <= 100);
    r
}

/// Helper: generate a bounded symbolic column.
fn any_col() -> u16 {
    let c: u16 = kani::any();
    kani::assume(c <= 200);
    c
}

/// Helper: generate a symbolic SelectionType.
fn any_selection_type() -> SelectionType {
    let v: u8 = kani::any();
    kani::assume(v < 4);
    match v {
        0 => SelectionType::Simple,
        1 => SelectionType::Block,
        2 => SelectionType::Semantic,
        _ => SelectionType::Lines,
    }
}

/// Helper: generate a symbolic SelectionSide.
fn any_side() -> SelectionSide {
    if kani::any() {
        SelectionSide::Left
    } else {
        SelectionSide::Right
    }
}

/// INV-SEL-31: TextSelection::new creates with None state
#[kani::proof]
fn text_selection_new_none_state() {
    let sel = TextSelection::new();

    kani::assert(sel.state() == SelectionState::None, "new is None");
    kani::assert(!sel.has_selection(), "new has no selection");
    kani::assert(!sel.is_complete(), "new is not complete");
    kani::assert(!sel.is_in_progress(), "new is not in progress");
}

/// INV-SEL-33: start_selection transitions from None to InProgress for any coords/type
#[kani::proof]
fn text_selection_start_transitions() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let side = any_side();
    let sel_type = any_selection_type();

    sel.start_selection(row, col, side, sel_type);

    kani::assert(
        sel.state() == SelectionState::InProgress,
        "start -> InProgress",
    );
    kani::assert(sel.has_selection(), "has selection after start");
    kani::assert(sel.is_in_progress(), "is in progress after start");
    kani::assert(!sel.is_complete(), "not complete after start");
}

/// INV-SEL-34: complete_selection transitions from InProgress to Complete
#[kani::proof]
fn text_selection_complete_transitions() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();

    sel.start_selection(row, col, any_side(), any_selection_type());
    sel.complete_selection();

    kani::assert(
        sel.state() == SelectionState::Complete,
        "complete -> Complete",
    );
    kani::assert(sel.is_complete(), "is complete");
    kani::assert(!sel.is_in_progress(), "not in progress");
}

/// INV-SEL-35: clear transitions to None from any state
#[kani::proof]
fn text_selection_clear_transitions_to_none() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let side = any_side();
    let sel_type = any_selection_type();

    // Choose which state to clear from
    let state_choice: u8 = kani::any();
    kani::assume(state_choice < 3);

    match state_choice {
        0 => { /* clear from None */ }
        1 => {
            sel.start_selection(row, col, side, sel_type);
        }
        _ => {
            sel.start_selection(row, col, side, sel_type);
            sel.complete_selection();
        }
    }

    sel.clear();
    kani::assert(sel.state() == SelectionState::None, "clear -> None");
}

/// INV-SEL-36: complete_selection only works from InProgress
#[kani::proof]
fn text_selection_complete_requires_in_progress() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();

    // complete_selection from None should have no effect
    sel.complete_selection();
    kani::assert(
        sel.state() == SelectionState::None,
        "complete from None = noop",
    );

    // complete_selection from Complete should have no effect
    sel.start_selection(row, col, any_side(), any_selection_type());
    sel.complete_selection();
    kani::assert(
        sel.state() == SelectionState::Complete,
        "first complete works",
    );

    sel.complete_selection(); // Call again
    kani::assert(
        sel.state() == SelectionState::Complete,
        "double complete = noop",
    );
}

/// INV-SEL-37: update_selection only updates during InProgress
#[kani::proof]
fn text_selection_update_requires_in_progress() {
    let mut sel = TextSelection::new();
    let r1 = any_row();
    let c1 = any_col();
    let r2 = any_row();
    let c2 = any_col();

    // Update before start should have no effect
    sel.update_selection(r2, c2, SelectionSide::Right);
    kani::assert(
        sel.state() == SelectionState::None,
        "update from None = noop",
    );

    // Update during InProgress should work
    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r2, c2, SelectionSide::Right);
    kani::assert(sel.end().row == r2, "update row");
    kani::assert(sel.end().col == c2, "update col");

    // Update after Complete should have no effect
    sel.complete_selection();
    sel.update_selection(r1, c1, SelectionSide::Left);
    kani::assert(sel.end().row == r2, "update from Complete = noop row");
    kani::assert(sel.end().col == c2, "update from Complete = noop col");
}

/// INV-SEL-38: extend_selection only works from Complete
#[kani::proof]
fn text_selection_extend_requires_complete() {
    let mut sel = TextSelection::new();
    let r1 = any_row();
    let c1 = any_col();
    let r2 = any_row();
    let c2 = any_col();

    // Extend from None should have no effect
    sel.extend_selection(r2, c2, SelectionSide::Right);
    kani::assert(
        sel.state() == SelectionState::None,
        "extend from None = noop",
    );

    // Extend from InProgress should have no effect
    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Simple);
    let end_before = sel.end();
    sel.extend_selection(r2, c2, SelectionSide::Right);
    kani::assert(
        sel.state() == SelectionState::InProgress,
        "extend from InProgress = noop state",
    );
    kani::assert(sel.end() == end_before, "extend from InProgress = noop end");

    // Extend from Complete should work and go back to InProgress
    sel.complete_selection();
    sel.extend_selection(r2, c2, SelectionSide::Right);
    kani::assert(
        sel.state() == SelectionState::InProgress,
        "extend from Complete -> InProgress",
    );
    kani::assert(sel.end().row == r2, "extend row");
    kani::assert(sel.end().col == c2, "extend col");
}

/// INV-SEL-40: selection_type preserved during transitions for any type
#[kani::proof]
fn text_selection_type_preserved() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let sel_type = any_selection_type();

    sel.start_selection(row, col, SelectionSide::Left, sel_type);
    kani::assert(
        sel.selection_type() == sel_type,
        "type preserved after start",
    );

    sel.update_selection(row, col, SelectionSide::Right);
    kani::assert(
        sel.selection_type() == sel_type,
        "type preserved after update",
    );

    sel.complete_selection();
    kani::assert(
        sel.selection_type() == sel_type,
        "type preserved after complete",
    );
}

/// INV-SEL-41: is_empty true when start equals end
#[kani::proof]
fn text_selection_is_empty() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();

    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Simple);
    kani::assert(sel.is_empty(), "empty right after start");

    sel.update_selection(row, col, SelectionSide::Left);
    kani::assert(sel.is_empty(), "empty when update to same pos");

    // Use a different column to make non-empty
    let col2 = any_col();
    kani::assume(col2 != col);
    sel.update_selection(row, col2, SelectionSide::Left);
    kani::assert(!sel.is_empty(), "non-empty when cols differ");
}

/// INV-SEL-42: has_selection false only in None state
#[kani::proof]
fn text_selection_has_selection() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();

    kani::assert(!sel.has_selection(), "no selection in None");

    sel.start_selection(row, col, any_side(), any_selection_type());
    kani::assert(sel.has_selection(), "has selection in InProgress");

    sel.complete_selection();
    kani::assert(sel.has_selection(), "has selection in Complete");

    sel.clear();
    kani::assert(!sel.has_selection(), "no selection after clear");
}

/// INV-SEL-47: Block selection contains all cells in rectangle for symbolic bounds
#[kani::proof]
fn text_selection_block_contains_rectangle() {
    let mut sel = TextSelection::new();

    let r1: i32 = kani::any();
    let c1: u16 = kani::any();
    let r2: i32 = kani::any();
    let c2: u16 = kani::any();
    kani::assume(r1 >= 0 && r1 <= 10);
    kani::assume(r2 >= 0 && r2 <= 10);
    kani::assume(c1 <= 50);
    kani::assume(c2 <= 50);
    kani::assume(r1 < r2); // ensure multi-row
    kani::assume(c1 < c2); // ensure multi-col

    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(r2, c2, SelectionSide::Right);

    // Test a symbolic point inside the rectangle
    let tr: i32 = kani::any();
    let tc: u16 = kani::any();
    kani::assume(tr >= r1 && tr <= r2);
    kani::assume(tc >= c1 && tc <= c2);
    kani::assert(sel.contains(tr, tc), "block contains interior point");

    // Test a point outside: column too small
    if c1 > 0 {
        kani::assert(!sel.contains(r1, c1 - 1), "block excludes left of rect");
    }
}

/// INV-SEL-48: Simple selection spans rows correctly with symbolic coords
#[kani::proof]
fn text_selection_simple_multiline() {
    let mut sel = TextSelection::new();

    let r1: i32 = kani::any();
    let c1: u16 = kani::any();
    let r2: i32 = kani::any();
    let c2: u16 = kani::any();
    kani::assume(r1 >= 0 && r1 <= 5);
    kani::assume(r2 >= 0 && r2 <= 5);
    kani::assume(c1 <= 50);
    kani::assume(c2 <= 50);
    kani::assume(r1 < r2); // multi-line: r1 < r2
    kani::assume(c2 > 0); // end col > 0 so we have content on last row

    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r2, c2, SelectionSide::Right);

    // Start row: start col is inside, before it is outside
    kani::assert(sel.contains(r1, c1), "start point selected");
    if c1 > 0 {
        kani::assert(!sel.contains(r1, c1 - 1), "before start not selected");
    }

    // Middle row: fully selected
    if r2 - r1 > 1 {
        kani::assert(sel.contains(r1 + 1, 0), "middle row start selected");
        kani::assert(sel.contains(r1 + 1, 100), "middle row end selected");
    }

    // End row: end col is last selected
    kani::assert(sel.contains(r2, c2), "end point selected");
    kani::assert(!sel.contains(r2, c2 + 1), "after end not selected");
}

/// INV-SEL-49: adjust_for_scroll updates row coordinates with symbolic values
#[kani::proof]
fn text_selection_adjust_for_scroll() {
    let mut sel = TextSelection::new();

    let r1 = any_row();
    let c1 = any_col();
    let r2 = any_row();
    let c2 = any_col();
    kani::assume(r1 >= 0 && r1 <= 50);
    kani::assume(r2 >= 0 && r2 <= 50);

    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r2, c2, SelectionSide::Right);
    sel.complete_selection();

    let delta: i32 = kani::any();
    kani::assume(delta >= -10 && delta <= 10);
    let screen_lines: i32 = 100;
    // Scrollback floor wide enough that the symbolic rows stay in-range.
    let floor: i32 = screen_lines - 1;

    let start_before = sel.start().row;
    let end_before = sel.end().row;

    let visible = sel.adjust_for_scroll(delta, screen_lines, floor);

    if visible {
        kani::assert(
            sel.start().row == start_before - delta,
            "scroll adjusts start row",
        );
        kani::assert(
            sel.end().row == end_before - delta,
            "scroll adjusts end row",
        );
    }
}

/// INV-SEL-50: adjust_for_scroll clears selection when scrolled off screen
#[kani::proof]
fn text_selection_adjust_clears_when_scrolled_off() {
    let mut sel = TextSelection::new();

    let r1: i32 = kani::any();
    let c1 = any_col();
    let r2: i32 = kani::any();
    let c2 = any_col();
    kani::assume(r1 >= 0 && r1 <= 20);
    kani::assume(r2 >= 0 && r2 <= 20);

    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r2, c2, SelectionSide::Right);
    sel.complete_selection();

    // Scroll far enough that the entire selection is below the history floor.
    let visible = sel.adjust_for_scroll(1000, 24, 23);

    kani::assert(!visible, "scrolled off = not visible");
    kani::assert(
        sel.state() == SelectionState::None,
        "scrolled off = cleared",
    );
}

/// INV-SEL-51: expand_semantic updates columns when in Semantic mode
#[kani::proof]
fn text_selection_expand_semantic() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let expand_start: u16 = kani::any();
    let expand_end: u16 = kani::any();
    kani::assume(expand_start <= 200);
    kani::assume(expand_end <= 200);

    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Semantic);
    sel.expand_semantic(expand_start, expand_end);

    kani::assert(sel.start().col == expand_start, "semantic expand start col");
    kani::assert(sel.end().col == expand_end, "semantic expand end col");
    kani::assert(
        sel.start().side == SelectionSide::Left,
        "semantic expand start side",
    );
    kani::assert(
        sel.end().side == SelectionSide::Right,
        "semantic expand end side",
    );
}

/// INV-SEL-52: expand_semantic only works in InProgress + Semantic
#[kani::proof]
fn text_selection_expand_semantic_guards() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let expand_start: u16 = kani::any();
    let expand_end: u16 = kani::any();
    kani::assume(expand_start <= 200);
    kani::assume(expand_end <= 200);

    // Should not expand in Simple mode
    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Simple);
    sel.expand_semantic(expand_start, expand_end);
    kani::assert(sel.start().col == col, "no expand in Simple");

    // Should work in Semantic mode
    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Semantic);
    sel.expand_semantic(expand_start, expand_end);
    kani::assert(sel.start().col == expand_start, "expand works in Semantic");

    // Should not expand after Complete
    sel.complete_selection();
    sel.expand_semantic(0, 0);
    kani::assert(sel.start().col == expand_start, "no expand after Complete");
}

/// INV-SEL-53: expand_lines sets full line boundaries with symbolic width
#[kani::proof]
fn text_selection_expand_lines() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let line_width: u16 = kani::any();
    kani::assume(line_width <= 500);

    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Lines);
    sel.expand_lines(line_width);

    kani::assert(sel.start().col == 0, "lines expand start = 0");
    kani::assert(sel.end().col == line_width, "lines expand end = width");
    kani::assert(
        sel.start().side == SelectionSide::Left,
        "lines expand start side",
    );
    kani::assert(
        sel.end().side == SelectionSide::Right,
        "lines expand end side",
    );
}

/// INV-SEL-54: expand_lines only works in InProgress + Lines
#[kani::proof]
fn text_selection_expand_lines_guards() {
    let mut sel = TextSelection::new();
    let row = any_row();
    let col = any_col();
    let line_width: u16 = kani::any();
    kani::assume(line_width <= 500);

    // Should not expand in Simple mode
    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Simple);
    sel.expand_lines(line_width);
    kani::assert(sel.start().col == col, "no expand in Simple");

    // Should work in Lines mode
    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Lines);
    sel.expand_lines(line_width);
    kani::assert(sel.start().col == 0, "expand works in Lines");
    kani::assert(sel.end().col == line_width, "expand end = width");
}

/// INV-SEL-57: `truncate_to_floor` clamps a single evicted endpoint onto the floor
/// and clears ONLY when the whole span is unreachable.
///
/// The three outcomes are exhaustive and are asserted as such: nothing evicted (rows
/// untouched, no report), everything evicted or no history at all (cleared), and one
/// endpoint evicted (the surviving row is preserved EXACTLY, the head lands on the
/// floor, the state machine does not move). The `floor == 0` arm is the alt-screen
/// guard: with no retained history there is no row to clamp onto and clamping would
/// pin the highlight to row 0 of a buffer the user never selected in.
#[kani::proof]
fn text_selection_truncate_to_floor_clamps_one_evicted_endpoint() {
    let mut sel = TextSelection::new();

    let r1 = any_row();
    let c1 = any_col();
    let r2 = any_row();
    let c2 = any_col();

    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r2, c2, SelectionSide::Right);
    sel.complete_selection();

    // Symbolic floor. The oldest retained row sits `floor` rows above the live top,
    // which is exactly the relation `Grid::oldest_absolute_row()` maintains, so the
    // relative floor the method derives is `-floor`.
    let floor: u64 = kani::any();
    kani::assume(floor <= 200);
    let oldest_abs: u64 = 1000;
    let live_top_abs = oldest_abs + floor;
    let min_row = -i32::try_from(floor).unwrap();

    let lo = if r1 <= r2 { r1 } else { r2 };
    let hi = if r1 <= r2 { r2 } else { r1 };

    let survived = sel.truncate_to_floor(oldest_abs, live_top_abs);

    if lo >= min_row {
        kani::assert(survived, "nothing evicted = nothing to lose");
        kani::assert(sel.start().row == r1, "untouched start row");
        kani::assert(sel.end().row == r2, "untouched end row");
        kani::assert(!sel.truncated(), "no loss to report");
    } else if hi < min_row || min_row == 0 {
        kani::assert(!survived, "no reachable content left");
        kani::assert(sel.state() == SelectionState::None, "both gone = cleared");
        kani::assert(!sel.truncated(), "a cleared selection reports nothing");
    } else if survived {
        kani::assert(sel.truncated(), "partial loss is reported");
        kani::assert(
            sel.state() == SelectionState::Complete,
            "a clamp does not move the state machine",
        );
        kani::assert(
            sel.normalized_start().row == min_row,
            "the evicted head clamps onto the oldest retained row",
        );
        kani::assert(
            sel.normalized_end().row == hi,
            "the retained endpoint's row is preserved exactly",
        );
    } else {
        // The only other outcome: the clamp left an empty span (the surviving anchor
        // was itself the floor row at col 0, side Left), which is a TOTAL loss and
        // clears rather than surviving as something nothing paints and nothing copies.
        kani::assert(sel.state() == SelectionState::None, "empty clamp clears");
    }
}

/// INV-SEL-58: the Block twin of INV-SEL-57 — a clamp moves the ROW only.
///
/// `normalized_start`/`normalized_end` take the min/max of rows and columns
/// INDEPENDENTLY for a Block, so forcing the clamped anchor's column to 0 would widen
/// the rectangle to column 0 across every retained row: a wrong-copy path rather than
/// a degradation.
#[kani::proof]
fn text_selection_truncate_to_floor_block_preserves_columns() {
    let mut sel = TextSelection::new();

    let r1 = any_row();
    let c1 = any_col();
    let r2 = any_row();
    let c2 = any_col();

    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(r2, c2, SelectionSide::Right);
    sel.complete_selection();

    let floor: u64 = kani::any();
    kani::assume(floor <= 200);
    let oldest_abs: u64 = 1000;
    let live_top_abs = oldest_abs + floor;

    let survived = sel.truncate_to_floor(oldest_abs, live_top_abs);

    if survived && sel.truncated() {
        kani::assert(sel.start().col == c1, "block start column preserved");
        kani::assert(sel.end().col == c2, "block end column preserved");
    }
}
