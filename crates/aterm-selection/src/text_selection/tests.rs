// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unit tests for text selection state machine.

use super::*;

#[test]
fn test_new_selection() {
    let sel = TextSelection::new();
    assert_eq!(sel.state(), SelectionState::None);
    assert!(!sel.has_selection());
}

#[test]
fn test_start_and_complete_selection() {
    let mut sel = TextSelection::new();

    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    assert_eq!(sel.state(), SelectionState::InProgress);
    assert!(sel.has_selection());
    assert!(sel.is_in_progress());

    sel.update_selection(0, 10, SelectionSide::Right);
    assert_eq!(sel.end().col, 10);

    sel.complete_selection();
    assert_eq!(sel.state(), SelectionState::Complete);
    assert!(sel.is_complete());
}

#[test]
fn test_clear_selection() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.complete_selection();
    assert!(sel.has_selection());

    sel.clear();
    assert!(!sel.has_selection());
    assert_eq!(sel.state(), SelectionState::None);
}

#[test]
fn test_contains_simple() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.contains(0, 5));
    assert!(sel.contains(0, 7));
    assert!(sel.contains(0, 10));
    assert!(!sel.contains(0, 4));
    assert!(!sel.contains(0, 11));
    assert!(!sel.contains(1, 7));
}

#[test]
fn test_contains_multiline() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 3, SelectionSide::Right);
    sel.complete_selection();

    // Row 0: from col 5 to end
    assert!(!sel.contains(0, 4));
    assert!(sel.contains(0, 5));
    assert!(sel.contains(0, 80)); // Full line selected after start

    // Row 1: full line
    assert!(sel.contains(1, 0));
    assert!(sel.contains(1, 80));

    // Row 2: from start to col 3
    assert!(sel.contains(2, 0));
    assert!(sel.contains(2, 3));
    assert!(!sel.contains(2, 4));
}

#[test]
fn test_contains_block() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();

    // Rectangular region: rows 0-2, cols 5-10
    assert!(sel.contains(0, 5));
    assert!(sel.contains(1, 7));
    assert!(sel.contains(2, 10));
    assert!(!sel.contains(0, 4));
    assert!(!sel.contains(0, 11));
    assert!(!sel.contains(3, 7));
}

#[test]
fn test_normalized_start_end() {
    let mut sel = TextSelection::new();
    // Select backwards
    sel.start_selection(5, 10, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(2, 3, SelectionSide::Left);
    sel.complete_selection();

    let ns = sel.normalized_start();
    let ne = sel.normalized_end();

    assert_eq!(ns.row, 2);
    assert_eq!(ns.col, 3);
    assert_eq!(ne.row, 5);
    assert_eq!(ne.col, 10);
}

#[test]
fn test_extend_selection() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    // Shift-click to extend
    sel.extend_selection(2, 15, SelectionSide::Right);
    assert_eq!(sel.state(), SelectionState::InProgress);
    assert_eq!(sel.end().row, 2);
    assert_eq!(sel.end().col, 15);
}

#[test]
fn test_extend_selection_preserves_anchor_cell_when_crossing_left() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 7, SelectionSide::Right);
    sel.complete_selection();

    sel.extend_selection(0, 0, SelectionSide::Left);
    sel.complete_selection();

    let bounds = sel
        .side_adjusted_bounds()
        .expect("cross-anchor extension should remain non-empty");
    assert_eq!(bounds, (0, 0, 0, 3));
    assert_eq!(sel.start().side, SelectionSide::Right);
}

#[test]
fn test_extend_selection_preserves_anchor_cell_when_crossing_right() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 10, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(0, 5, SelectionSide::Left);
    sel.complete_selection();

    sel.extend_selection(0, 15, SelectionSide::Right);
    sel.complete_selection();

    let bounds = sel
        .side_adjusted_bounds()
        .expect("cross-anchor extension should remain non-empty");
    assert_eq!(bounds, (0, 10, 0, 15));
    assert_eq!(sel.start().side, SelectionSide::Left);
}

#[test]
fn test_anchor_ordering() {
    let a1 = SelectionAnchor::new(0, 5, SelectionSide::Left);
    let a2 = SelectionAnchor::new(0, 5, SelectionSide::Right);
    let a3 = SelectionAnchor::new(0, 6, SelectionSide::Left);
    let a4 = SelectionAnchor::new(1, 0, SelectionSide::Left);

    assert!(a1 < a2);
    assert!(a2 < a3);
    assert!(a3 < a4);
}

#[test]
fn test_adjust_for_scroll_shifts_coordinates() {
    let mut sel = TextSelection::new();
    sel.start_selection(5, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(7, 10, SelectionSide::Right);
    sel.complete_selection();

    // Scroll up by 2: content rows shift down (delta=2).
    // floor = 23 (max_rows - 1) reproduces the historical -23 lower bound.
    let visible = sel.adjust_for_scroll(2, 24, 23);
    assert!(visible);
    assert_eq!(sel.normalized_start().row, 3); // 5 - 2
    assert_eq!(sel.normalized_end().row, 5); // 7 - 2
    // Columns unchanged
    assert_eq!(sel.normalized_start().col, 3);
    assert_eq!(sel.normalized_end().col, 10);
}

#[test]
fn test_adjust_for_scroll_clears_when_offscreen() {
    let mut sel = TextSelection::new();
    sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 5, SelectionSide::Right);
    sel.complete_selection();

    // Large scroll pushes selection below the history floor (-23)
    let visible = sel.adjust_for_scroll(100, 24, 23);
    assert!(!visible);
    assert!(!sel.has_selection());
}

#[test]
fn test_adjust_for_scroll_noop_when_no_selection() {
    let mut sel = TextSelection::new();
    let visible = sel.adjust_for_scroll(5, 24, 23);
    assert!(visible); // No selection => returns true (nothing to clear)
    assert!(!sel.has_selection());
}

#[test]
fn test_translate_rows_for_presentation_preserves_selection_semantics() {
    let mut sel = TextSelection::new();
    sel.start_selection(7, 19, SelectionSide::Right, SelectionType::Semantic);
    sel.update_selection(-3, 2, SelectionSide::Left);
    sel.complete_selection();

    sel.translate_rows_for_presentation(11);

    assert_eq!(sel.start().row, 18);
    assert_eq!(sel.end().row, 8);
    assert_eq!(sel.start().col, 19);
    assert_eq!(sel.end().col, 2);
    assert_eq!(sel.start().side, SelectionSide::Right);
    assert_eq!(sel.end().side, SelectionSide::Left);
    assert_eq!(sel.selection_type(), SelectionType::Semantic);
    assert_eq!(sel.state(), SelectionState::Complete);
}

#[test]
fn test_translate_rows_for_presentation_saturates_at_i32_boundaries() {
    let mut upper = TextSelection::new();
    upper.start_selection(i32::MAX - 1, 3, SelectionSide::Left, SelectionType::Block);
    upper.update_selection(i32::MAX, 9, SelectionSide::Right);
    upper.translate_rows_for_presentation(2);
    assert_eq!(upper.start().row, i32::MAX);
    assert_eq!(upper.end().row, i32::MAX);
    assert_eq!((upper.start().col, upper.end().col), (3, 9));

    let mut lower = TextSelection::new();
    lower.start_selection(i32::MIN + 1, 4, SelectionSide::Right, SelectionType::Lines);
    lower.update_selection(i32::MIN, 10, SelectionSide::Left);
    lower.translate_rows_for_presentation(-2);
    assert_eq!(lower.start().row, i32::MIN);
    assert_eq!(lower.end().row, i32::MIN);
    assert_eq!((lower.start().col, lower.end().col), (4, 10));
}

#[test]
fn test_adjust_for_scroll_large_delta_no_overflow() {
    // Regression: i32::MAX delta (region scroll sentinel) with negative row
    // must not panic from arithmetic overflow.
    let mut sel = TextSelection::new();
    sel.start_selection(-5, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-3, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(i32::MAX, 24, 23);
    assert!(!visible, "i32::MAX delta must clear selection");
    assert!(!sel.has_selection());
}

#[test]
fn test_adjust_for_scroll_boundary_just_visible() {
    // With floor=23, min_row = -floor = -23.
    // Selection at row 0, delta 23 => new_start_row = 0 - 23 = -23 = min_row.
    // Should still be visible (boundary is inclusive: `< min_row` clears).
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(23, 24, 23);
    assert!(visible, "row -23 is exactly min_row and should be visible");
    assert!(sel.has_selection());
    assert_eq!(sel.normalized_start().row, -23);
}

#[test]
fn test_adjust_for_scroll_boundary_just_offscreen() {
    // With floor=23, min_row = -23.
    // Selection at row 0, delta 24 => new_start_row = 0 - 24 = -24 < min_row.
    // Should be cleared.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(24, 24, 23);
    assert!(!visible, "row -24 is below min_row and should clear");
    assert!(!sel.has_selection());
}

#[test]
fn test_adjust_for_scroll_in_progress_selection() {
    // Scroll adjustment must work on InProgress selections too,
    // not just Complete ones.
    let mut sel = TextSelection::new();
    sel.start_selection(5, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(7, 10, SelectionSide::Right);
    // Deliberately do NOT call complete_selection()
    assert!(sel.is_in_progress());

    let visible = sel.adjust_for_scroll(2, 24, 23);
    assert!(visible);
    assert_eq!(sel.normalized_start().row, 3); // 5 - 2
    assert_eq!(sel.normalized_end().row, 5); // 7 - 2
    assert!(sel.is_in_progress(), "state should remain InProgress");
}

#[test]
fn test_adjust_for_scroll_negative_delta() {
    // Negative delta = content shifted up = selection rows increase.
    // saturating_sub(-3) on row 5 = 5 - (-3) = 8.
    let mut sel = TextSelection::new();
    sel.start_selection(5, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(7, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(-3, 24, 23);
    assert!(visible);
    assert_eq!(sel.normalized_start().row, 8); // 5 - (-3)
    assert_eq!(sel.normalized_end().row, 10); // 7 - (-3)
}

#[test]
fn test_adjust_for_scroll_negative_delta_pushes_past_max() {
    // With max_rows=24, max_row = 24.
    // Selection at row 20, delta -5 => new row = 20 - (-5) = 25 > max_row.
    // Should be cleared.
    let mut sel = TextSelection::new();
    sel.start_selection(20, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(22, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(-5, 24, 23);
    assert!(!visible, "row 25 exceeds max_row and should clear");
    assert!(!sel.has_selection());
}

#[test]
fn test_adjust_for_scroll_exact_max_row_boundary() {
    // With max_rows=24, max_row = 24 (one past last visible row index 23).
    // Selection at row 20, delta -4 => new row = 20 - (-4) = 24 = max_row.
    // The check is `> max_row`, so row 24 exactly is still considered visible.
    // This documents current behavior: max_row is inclusive upper bound.
    let mut sel = TextSelection::new();
    sel.start_selection(20, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(20, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(-4, 24, 23);
    assert!(
        visible,
        "row 24 == max_row should be considered visible (inclusive upper bound)"
    );
    assert!(sel.has_selection());
    assert_eq!(sel.normalized_start().row, 24);
}

#[test]
fn test_adjust_for_scroll_exact_min_row_boundary() {
    // With floor=23, min_row = -floor = -23.
    // Selection at row 0, delta 23 => new row = 0 - 23 = -23 = min_row.
    // The check is `< min_row`, so row -23 exactly is still considered visible.
    // Complements test_adjust_for_scroll_boundary_just_visible by checking
    // from the min_row perspective with explicit boundary arithmetic.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    // Verify boundary is inclusive
    assert!(sel.adjust_for_scroll(23, 24, 23));
    assert_eq!(sel.normalized_start().row, -23);

    // One more clears it
    let mut sel2 = TextSelection::new();
    sel2.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel2.update_selection(0, 10, SelectionSide::Right);
    sel2.complete_selection();
    assert!(!sel2.adjust_for_scroll(24, 24, 23));
    assert!(!sel2.has_selection());
}

#[test]
fn test_adjust_for_scroll_asymmetric_selection_span() {
    // Selection spanning both positive and negative rows after scroll.
    // Start at row 5, end at row 10 in a 24-row terminal.
    // Delta 8 => start at row -3 (scrollback), end at row 2 (visible).
    // Both within bounds: min_row=-23 (floor=23), max_row=24.
    let mut sel = TextSelection::new();
    sel.start_selection(5, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(10, 10, SelectionSide::Right);
    sel.complete_selection();

    let visible = sel.adjust_for_scroll(8, 24, 23);
    assert!(visible);
    assert_eq!(sel.normalized_start().row, -3); // 5 - 8
    assert_eq!(sel.normalized_end().row, 2); // 10 - 8
}

#[test]
fn test_adjust_for_scroll_single_row_terminal() {
    // Edge case: 1-row terminal with no scrollback. floor=0 => min_row = 0,
    // max_row = 1. This means only rows 0 and 1 are valid.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 5, SelectionSide::Right);
    sel.complete_selection();

    // Delta 1 => row -1 < min_row(0) => cleared
    let visible = sel.adjust_for_scroll(1, 1, 0);
    assert!(!visible, "1-row terminal: row -1 is below min_row 0");
    assert!(!sel.has_selection());
}

#[test]
fn test_adjust_for_scroll_deep_scrollback_survives_then_evicts() {
    // Regression (#4056 / SCR-1): a selection made deep in scrollback uses
    // live-screen coords where scrollback is negative. With the lower clear
    // bound derived from the visible height (-(max_rows-1) = -23), a single
    // line of background output would scroll the selection past -23 and wipe
    // it even though SCR-1 view pinning keeps the exact same text on screen.
    // The clear bound must instead be the retained-history floor (-floor).
    //
    // Scenario: 24-row screen, 500 lines of scrollback, selection anchored
    // ~40 lines back into history (rows -40 and -38).
    let max_rows = 24;
    let floor = 500;
    let mut sel = TextSelection::new();
    sel.start_selection(-40, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-38, 10, SelectionSide::Right);
    sel.complete_selection();

    // A single line of output (delta = 1) must NOT clear the selection:
    // -41 and -39 are still far above the history floor (-500).
    let visible = sel.adjust_for_scroll(1, max_rows, floor);
    assert!(
        visible,
        "deep-scrollback selection must survive a 1-line content scroll"
    );
    assert!(sel.has_selection());
    assert_eq!(sel.normalized_start().row, -41); // -40 - 1
    assert_eq!(sel.normalized_end().row, -39); // -38 - 1

    // Scrolling the START endpoint below the retained-history floor (its row would
    // drop to -41 - 460 = -501 < -500) evicts that endpoint's content — but NOT the
    // end's, which is still retained at -499. Destroying the whole selection there
    // threw away a span the user can still see; it now clamps the head onto the
    // oldest retained row and reports the loss.
    let visible = sel.adjust_for_scroll(460, max_rows, floor);
    assert!(
        visible,
        "one evicted endpoint is PARTIAL eviction: the retained half survives"
    );
    assert!(sel.has_selection());
    assert_eq!(
        sel.normalized_start().row,
        -500,
        "the evicted head clamps onto the oldest retained row"
    );
    assert_eq!(
        sel.normalized_start().col,
        0,
        "the clamped head starts at the beginning of that row"
    );
    assert_eq!(
        sel.normalized_end().row,
        -499,
        "the retained end does not move"
    );
    assert!(sel.truncated(), "the partial loss is reported, not hidden");
}

/// The non-vacuity control for the test above: once BOTH endpoints fall below the
/// floor there is nothing left to clamp onto, and the honest answer is still a clear.
#[test]
fn test_adjust_for_scroll_clears_when_both_endpoints_are_evicted() {
    let max_rows = 24;
    let floor = 500;
    let mut sel = TextSelection::new();
    sel.start_selection(-40, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-38, 10, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.adjust_for_scroll(1, max_rows, floor));

    // -641 and -639 are both past the -500 floor.
    assert!(!sel.adjust_for_scroll(600, max_rows, floor));
    assert!(!sel.has_selection());
    assert!(
        !sel.truncated(),
        "a cleared selection reports no partial loss"
    );
}

/// THE ALT-SCREEN GUARD. With `floor == 0` there is no retained history, so `min_row`
/// is 0 and there is no oldest RETAINED row to clamp onto — the alt grid is always
/// `Grid::with_scrollback(rows, cols, 0)`. Clamping here would pin the highlight to
/// alt row 0, content the user never selected, and a full-screen alt scroll takes the
/// uniform-delta path and records no damage band that could catch it afterwards.
///
/// This is the shape no test covered: only the LOWER endpoint evicted. The
/// single-row-terminal case cannot detect the guard's removal because both of its
/// anchors go below the floor together and take the both-gone arm either way.
#[test]
fn test_adjust_for_scroll_zero_floor_clears_rather_than_clamping_to_row_zero() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 7, SelectionSide::Right);
    sel.complete_selection();

    // delta 1 => rows -1 and 1: the lower endpoint alone is below min_row 0.
    let visible = sel.adjust_for_scroll(1, 24, 0);
    assert!(
        !visible,
        "with no retained history there is nothing to clamp to"
    );
    assert!(
        !sel.has_selection(),
        "clamping to row 0 here would leave a highlight over content the user \
         never selected"
    );
    assert!(!sel.truncated());
}

/// The evicted endpoint is not necessarily `start`: an upward drag leaves the older
/// row in `end`, and the clamp must follow the row order, not the field name.
#[test]
fn test_adjust_for_scroll_clamps_the_evicted_end_anchor_on_an_upward_drag() {
    let mut sel = TextSelection::new();
    sel.start_selection(-38, 10, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(-40, 4, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.adjust_for_scroll(461, 24, 500));
    assert_eq!(sel.start().row, -499, "the newer anchor rides the scroll");
    assert_eq!(
        sel.end().row,
        -500,
        "the older anchor clamps onto the floor"
    );
    assert_eq!(sel.end().col, 0);
    assert_eq!(
        sel.end().side,
        SelectionSide::Left,
        "a Right side on the clamped head would eat the first cell of the row"
    );
    assert!(sel.truncated());
}

/// BLOCK selections clamp the ROW only. `normalized_start`/`normalized_end` take the
/// min/max of rows and cols INDEPENDENTLY, so forcing the clamped anchor to column 0
/// would widen the rectangle to column 0 across every retained row — a wrong-copy
/// path rather than a degradation.
#[test]
fn test_adjust_for_scroll_block_clamp_preserves_columns() {
    let mut sel = TextSelection::new();
    sel.start_selection(-40, 12, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(-38, 20, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.adjust_for_scroll(461, 24, 500));
    assert_eq!(sel.start().row, -500, "the evicted row clamps");
    assert_eq!(
        (sel.start().col, sel.end().col),
        (12, 20),
        "a block keeps its column span; widening it to 0 would copy unselected text"
    );
    assert!(sel.truncated());
}

/// A clamp that leaves nothing selectable is a TOTAL loss, not a partial one, and
/// must not survive as a selection nothing paints and nothing copies. The surviving
/// anchor here is itself `(min_row, 0, Left)`, so side adjustment retreats the end to
/// `(min_row - 1, u16::MAX)` and the span is empty the instant the head clamps onto it.
#[test]
fn test_adjust_for_scroll_clears_when_the_clamp_collapses_the_span() {
    let mut sel = TextSelection::new();
    sel.start_selection(-49, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-50, 0, SelectionSide::Left);
    sel.complete_selection();

    // delta 1 => -50 and -51 against a floor of 50: the older anchor clamps onto
    // -50, which is exactly where the surviving one already sits.
    assert!(!sel.adjust_for_scroll(1, 24, 50));
    assert!(
        !sel.has_selection(),
        "an empty clamped span is the honest total-loss clear, not a ghost selection"
    );
    assert!(!sel.truncated());
}

/// The truncation report is per-SPAN, not sticky: `aterm-control`'s conformance check
/// compares the `selection` reply header exactly, so a flag that outlived its
/// selection would append ` incomplete` to every later reply.
#[test]
fn test_truncated_resets_on_new_selection_and_on_clear() {
    let mut sel = TextSelection::new();
    sel.start_selection(-40, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-38, 10, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.adjust_for_scroll(461, 24, 500));
    assert!(sel.truncated());

    sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
    assert!(!sel.truncated(), "a fresh span has lost nothing");

    sel.update_selection(4, 4, SelectionSide::Right);
    sel.complete_selection();
    assert!(sel.adjust_for_scroll(503, 24, 500));
    assert!(sel.truncated());
    sel.clear();
    assert!(!sel.truncated(), "no span left to be partial");
}

#[test]
fn test_adjust_for_row_splice_moves_history_endpoint_but_keeps_footer_endpoint() {
    let mut sel = TextSelection::new();
    sel.start_selection(1, 3, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(4, 10, SelectionSide::Right);
    sel.complete_selection();

    let mut old_clear_all = sel.clone();
    assert!(!old_clear_all.adjust_for_scroll(i32::MAX, 5, 8));
    assert!(
        !old_clear_all.has_selection(),
        "negative control reproduces the old generic region-scroll clear"
    );

    assert!(sel.adjust_for_row_splice(3, 1, 5, 8));
    assert_eq!(
        sel.start().row,
        0,
        "scroll-region content moves toward history"
    );
    assert_eq!(
        sel.end().row,
        4,
        "protected-footer content stays on its screen row"
    );
    assert_eq!(sel.start().col, 3);
    assert_eq!(sel.end().col, 10);
    assert!(
        sel.is_complete(),
        "the selection lifecycle state is preserved"
    );
}

#[test]
fn test_adjust_for_row_splice_handles_reverse_and_in_progress_anchors() {
    let mut sel = TextSelection::new();
    sel.start_selection(4, 9, SelectionSide::Right, SelectionType::Block);
    sel.update_selection(-2, 1, SelectionSide::Left);

    assert!(sel.adjust_for_row_splice(3, 2, 5, 8));
    assert_eq!(sel.start().row, 4, "raw footer start anchor stays fixed");
    assert_eq!(
        sel.end().row,
        -4,
        "raw history end anchor moves by the insertion"
    );
    assert!(sel.is_in_progress());
    assert_eq!(sel.selection_type(), SelectionType::Block);
}

#[test]
fn test_adjust_for_row_splice_clears_only_after_real_history_eviction() {
    let mut retained = TextSelection::new();
    retained.start_selection(-4, 0, SelectionSide::Left, SelectionType::Simple);
    retained.update_selection(-2, 2, SelectionSide::Right);
    retained.complete_selection();
    assert!(retained.adjust_for_row_splice(3, 1, 5, 5));
    assert_eq!(retained.normalized_start().row, -5);

    // One endpoint evicted by the splice is PARTIAL eviction and clamps, exactly as
    // in `adjust_for_scroll`. Routing the splice through a byte-identical below-floor
    // clear would have left a top-anchored archival splice destroying a whole
    // selection for one evicted endpoint.
    let mut partly_evicted = TextSelection::new();
    partly_evicted.start_selection(-5, 0, SelectionSide::Left, SelectionType::Simple);
    partly_evicted.update_selection(-2, 2, SelectionSide::Right);
    partly_evicted.complete_selection();
    assert!(partly_evicted.adjust_for_row_splice(3, 1, 5, 5));
    assert!(partly_evicted.has_selection());
    assert_eq!(partly_evicted.normalized_start().row, -5);
    assert_eq!(partly_evicted.normalized_end().row, -3);
    assert!(partly_evicted.truncated());

    // Both endpoints gone is still a clear — the non-vacuity control.
    let mut evicted = TextSelection::new();
    evicted.start_selection(-5, 0, SelectionSide::Left, SelectionType::Simple);
    evicted.update_selection(-4, 2, SelectionSide::Right);
    evicted.complete_selection();
    assert!(!evicted.adjust_for_row_splice(3, 2, 5, 5));
    assert!(!evicted.has_selection());
}

/// Proves that precomputing `normalized_bounds()` once and using the shared
/// `selection_contains_linear` function produces identical results to calling
/// `TextSelection::contains()` per-cell.
///
/// This validates the correctness invariant for the render-loop optimization:
/// instead of calling `contains()` per cell (which recomputes `normalized_start`
/// and `normalized_end` on every invocation), render paths should call
/// `normalized_bounds()` once and use `PrecomputedSelectionBounds::contains()`.
///
/// Related: #3179 finding #5 (claimed fixed, but renderer.rs, build.rs,
/// instanced.rs still use per-cell `contains()` instead of precomputed bounds).
#[test]
fn precomputed_bounds_equivalent_to_contains_linear() {
    let rows = 12_i32;
    let cols = 40_u16;

    let mut sel = TextSelection::new();
    // Forward selection: row 3 col 10 → row 8 col 25
    sel.start_selection(3, 10, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(8, 25, SelectionSide::Right);
    sel.complete_selection();

    let (start_row, start_col, end_row, end_col) = sel.normalized_bounds();

    for row in 0..rows {
        for col in 0..cols {
            let via_contains = sel.contains(row, col);
            let via_precomputed = aterm_types::selection::selection_contains_linear(
                row,
                usize::from(col),
                start_row,
                usize::from(start_col),
                end_row,
                usize::from(end_col),
            );
            assert_eq!(
                via_contains, via_precomputed,
                "linear mismatch at ({row}, {col})"
            );
        }
    }
}

/// Same equivalence test for backward (end < start) selection.
#[test]
fn precomputed_bounds_equivalent_to_contains_backward() {
    let rows = 8_i32;
    let cols = 30_u16;

    let mut sel = TextSelection::new();
    // Backward drag: start at row 6 col 20, drag up to row 1 col 5
    sel.start_selection(6, 20, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(1, 5, SelectionSide::Left);
    sel.complete_selection();

    let (start_row, start_col, end_row, end_col) = sel.normalized_bounds();

    for row in 0..rows {
        for col in 0..cols {
            let via_contains = sel.contains(row, col);
            let via_precomputed = aterm_types::selection::selection_contains_linear(
                row,
                usize::from(col),
                start_row,
                usize::from(start_col),
                end_row,
                usize::from(end_col),
            );
            assert_eq!(
                via_contains, via_precomputed,
                "backward linear mismatch at ({row}, {col})"
            );
        }
    }
}

/// Equivalence test for block (rectangular) selection.
#[test]
fn precomputed_bounds_equivalent_to_contains_block() {
    let rows = 10_i32;
    let cols = 50_u16;

    let mut sel = TextSelection::new();
    // Block selection: start row 2 col 35, end row 7 col 10
    // (reversed columns to test normalization)
    sel.start_selection(2, 35, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(7, 10, SelectionSide::Right);
    sel.complete_selection();

    let (start_row, start_col, end_row, end_col) = sel.normalized_bounds();

    for row in 0..rows {
        for col in 0..cols {
            let via_contains = sel.contains(row, col);
            let via_precomputed = aterm_types::selection::selection_contains_block(
                row,
                usize::from(col),
                start_row,
                usize::from(start_col),
                end_row,
                usize::from(end_col),
            );
            assert_eq!(
                via_contains, via_precomputed,
                "block mismatch at ({row}, {col})"
            );
        }
    }
}

// ── project_range tests ──

#[test]
fn test_project_range_no_selection() {
    let sel = TextSelection::new();
    assert_eq!(sel.project_range(79), None);
}

#[test]
fn test_project_range_simple_left_to_right() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_row, 0);
    assert_eq!(proj.start_col, 5);
    assert_eq!(proj.end_row, 0);
    assert_eq!(proj.end_col, 10);
    assert!(!proj.is_block);
}

#[test]
fn test_project_range_side_adjustment_right_start() {
    // Start on Right side of col 5 → effective start at col 6.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_col, 6, "Right-sided start shifts col forward");
    assert_eq!(proj.end_col, 10);
}

#[test]
fn test_project_range_side_adjustment_left_end() {
    // End on Left side of col 10 → effective end at col 9.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Left);

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_col, 5);
    assert_eq!(proj.end_col, 9, "Left-sided end shifts col backward");
}

#[test]
fn test_project_range_empty_after_side_adjustment() {
    // Start Right of col 5, end Left of col 6: effective range [6, 5] → empty.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(0, 6, SelectionSide::Left);

    assert_eq!(
        sel.project_range(79),
        None,
        "side adjustment yields empty range"
    );
}

#[test]
fn test_project_range_lines_expands_columns() {
    let mut sel = TextSelection::new();
    sel.start_selection(1, 5, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(3, 2, SelectionSide::Right);

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_col, 0, "Lines start at column 0");
    assert_eq!(proj.end_col, 79, "Lines end at last_col");
    assert_eq!(proj.start_row, 1);
    assert_eq!(proj.end_row, 3);
    assert!(!proj.is_block);
}

#[test]
fn test_project_range_block() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);

    let proj = sel.project_range(79).expect("should project");
    assert!(proj.is_block);
    assert_eq!(proj.start_row, 0);
    assert_eq!(proj.end_row, 2);
    assert_eq!(proj.start_col, 5);
    assert_eq!(proj.end_col, 10);
}

#[test]
fn test_project_range_backward_selection() {
    // Drag from row 5 col 10 backward to row 2 col 3.
    let mut sel = TextSelection::new();
    sel.start_selection(5, 10, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(2, 3, SelectionSide::Left);

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_row, 2);
    assert_eq!(proj.start_col, 3);
    assert_eq!(proj.end_row, 5);
    assert_eq!(proj.end_col, 10);
}

#[test]
fn test_project_range_multiline() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 3, SelectionSide::Right);

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_row, 0);
    assert_eq!(proj.start_col, 5);
    assert_eq!(proj.end_row, 2);
    assert_eq!(proj.end_col, 3);
}

// ── include_all tests ──

#[test]
fn test_include_all_expands_sides() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Right, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Left);

    // Before include_all: Right start → col 6, Left end → col 9
    let proj_before = sel.project_range(79).expect("before");
    assert_eq!(proj_before.start_col, 6);
    assert_eq!(proj_before.end_col, 9);

    sel.include_all();

    // After include_all: Left start → col 5, Right end → col 10
    let proj_after = sel.project_range(79).expect("after");
    assert_eq!(proj_after.start_col, 5);
    assert_eq!(proj_after.end_col, 10);
}

#[test]
fn test_include_all_noop_on_no_selection() {
    let mut sel = TextSelection::new();
    sel.include_all(); // should not panic
    assert!(!sel.has_selection());
}

// ── contains_cell wide character tests ──

#[test]
fn test_contains_cell_block_wide_char_start_at_left_boundary() {
    // Block selection cols 5..=10. A CJK char starts at col 4 (wide start,
    // continuation at col 5). The continuation at col 5 is inside the block,
    // so the wide char at col 4 should be selected.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();

    // Wide char at col 4: col 4 is outside block, but col 5 (continuation) is inside.
    // contains_cell with is_wide=true checks col 4 OR col 5 -> should be true.
    assert!(
        sel.contains_cell(1, 4, true, false),
        "wide char at col 4 should be selected (continuation at col 5 is in block)"
    );
    assert!(
        sel.contains_cell(1, 5, false, true),
        "the continuation already inside the block must stay selected too"
    );

    // Wide char at col 3: col 3 outside, col 4 also outside -> not selected.
    assert!(
        !sel.contains_cell(1, 3, true, false),
        "wide char at col 3 should NOT be selected (col 3 and col 4 both outside block)"
    );
}

#[test]
fn test_contains_cell_block_wide_continuation_at_right_boundary() {
    // Block selection cols 5..=10. A CJK char starts at col 10 (wide start,
    // continuation at col 11). The start at col 10 is inside the block.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();

    // Continuation cell at col 11: is_wide_continuation=true, checks col 10 -> inside.
    assert!(
        sel.contains_cell(1, 11, false, true),
        "continuation at col 11 should be selected (wide char start at col 10 is in block)"
    );

    // Continuation cell at col 12: checks col 11 -> outside.
    assert!(
        !sel.contains_cell(1, 12, false, true),
        "continuation at col 12 should NOT be selected (col 11 is outside block)"
    );
}

#[test]
fn test_contains_cell_block_wide_char_fully_inside() {
    // Block selection cols 5..=10. Wide char at col 7 (continuation at col 8).
    // Both columns inside the block.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.contains_cell(1, 7, true, false));
    assert!(sel.contains_cell(1, 8, false, true));
}

#[test]
fn test_contains_cell_block_wide_char_fully_outside() {
    // Block selection cols 5..=10. Wide char at col 12 (continuation at col 13).
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(!sel.contains_cell(1, 12, true, false));
    assert!(!sel.contains_cell(1, 13, false, true));
}

/// A drag that STARTS on the right half of a wide glyph must still paint the
/// left half.
///
/// The copy path admits the whole glyph whenever either of its cells is in
/// range — it has no way to emit half a cluster — so a highlight that stopped at
/// the bare start column would put a character on the clipboard that the user
/// never saw highlighted. A linear selection snaps a glyph whole for that
/// reason, exactly as Block does: both ask `glyph_cell_span`, so the selection
/// kind cannot change where a glyph's boundary lies.
#[test]
fn a_linear_drag_starting_on_a_wide_glyphs_continuation_still_paints_its_lead() {
    let mut sel = TextSelection::new();
    // The glyph occupies cols 4-5; the drag starts on its continuation (col 5).
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(
        sel.contains_cell(0, 4, true, false),
        "the lead of a glyph whose continuation is selected is selected too — \
         the copy takes that character whether or not it is painted"
    );
    assert!(
        sel.contains_cell(0, 5, false, true),
        "and its continuation, which is what the drag actually landed on"
    );
    // A whole glyph one column further left is untouched: cols 2 and 3 are both
    // outside, so nothing snaps it in.
    assert!(
        !sel.contains_cell(0, 2, true, false),
        "snapping reaches exactly one cell, never a glyph that is fully outside"
    );
    assert!(sel.contains_cell(0, 6, false, false));
}

/// The mirror at the far edge: a drag that STOPS on the left half of a wide
/// glyph must still paint the right half, or the band ends mid-character while
/// the clipboard receives the whole one.
#[test]
fn a_linear_drag_ending_on_a_wide_glyphs_lead_still_paints_its_continuation() {
    let mut sel = TextSelection::new();
    // The glyph occupies cols 10-11; the drag stops on its lead (col 10).
    sel.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    assert!(
        sel.contains_cell(0, 10, true, false),
        "the lead is selected"
    );
    assert!(
        sel.contains_cell(0, 11, false, true),
        "and so is the continuation of the glyph the drag stopped inside"
    );
    assert!(
        !sel.contains_cell(0, 12, true, false),
        "the glyph after it is fully outside and stays that way"
    );
}

/// Anchor order is not selection order: dragging RIGHT-to-LEFT lands the same
/// mid-glyph edges on the opposite anchors, and `normalized_start`/`_end` order
/// them before the snap. Both edges must behave exactly as in the forward drag.
#[test]
fn a_backwards_drag_snaps_the_same_wide_glyph_edges_as_a_forwards_one() {
    let mut forwards = TextSelection::new();
    forwards.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    forwards.update_selection(0, 10, SelectionSide::Right);
    forwards.complete_selection();

    let mut backwards = TextSelection::new();
    backwards.start_selection(0, 10, SelectionSide::Right, SelectionType::Simple);
    backwards.update_selection(0, 5, SelectionSide::Left);
    backwards.complete_selection();

    for (col, wide, cont) in [
        (4u16, true, false),
        (5, false, true),
        (10, true, false),
        (11, false, true),
        (2, true, false),
        (12, true, false),
    ] {
        assert_eq!(
            backwards.contains_cell(0, col, wide, cont),
            forwards.contains_cell(0, col, wide, cont),
            "col {col} must paint the same whichever direction the drag ran"
        );
    }
    assert!(backwards.contains_cell(0, 4, true, false));
    assert!(backwards.contains_cell(0, 11, false, true));
}

/// A `Lines` selection owns whole rows, so column snapping cannot change its
/// answer — pinned so the shared span helper never leaks a column condition into
/// the one selection kind that has none.
#[test]
fn a_lines_selection_paints_every_half_of_every_glyph_on_its_rows() {
    let mut sel = TextSelection::new();
    sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(2, 79, SelectionSide::Right);
    sel.complete_selection();

    assert!(sel.contains_cell(1, 0, true, false));
    assert!(sel.contains_cell(1, 1, false, true));
    assert!(sel.contains_cell(2, 79, true, false));
    assert!(!sel.contains_cell(3, 0, true, false), "rows below are not");
    assert!(!sel.contains_cell(0, 79, true, false), "nor rows above");
}

#[test]
fn test_contains_cell_block_continuation_at_col_zero() {
    // Edge case: continuation at column 0 (shouldn't happen in practice,
    // but must not underflow).
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 5, SelectionSide::Right);
    sel.complete_selection();

    // A continuation at col 0 has no lead to back up to; `glyph_cell_span`
    // saturates to (0, 0) rather than wrapping to u16::MAX.
    assert!(sel.contains_cell(1, 0, false, true));
}

#[test]
fn test_contains_cell_no_selection() {
    let sel = TextSelection::new();
    assert!(!sel.contains_cell(0, 5, true, false));
    assert!(!sel.contains_cell(0, 5, false, true));
    assert!(!sel.contains_cell(0, 5, false, false));
}

// ── column 0 boundary tests (issue #7623) ──

/// Regression: selection ending at col 0 with Left side incorrectly included
/// column 0 because the `ne.col > 0` guard skipped the side adjustment,
/// leaving end_col = 0 instead of retreating to the previous row.
#[test]
fn test_contains_end_left_side_col_zero_multiline() {
    // Selection from (0, 5, Left) to (2, 0, Left).
    // Left-sided end at col 0 means "stop before col 0 on row 2",
    // so nothing on row 2 should be selected.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 0, SelectionSide::Left);
    sel.complete_selection();

    // Row 0: cols 5+ selected
    assert!(!sel.contains(0, 4));
    assert!(sel.contains(0, 5));
    assert!(sel.contains(0, 80));

    // Row 1: entirely selected (middle row)
    assert!(sel.contains(1, 0));
    assert!(sel.contains(1, 40));
    assert!(sel.contains(1, 80));

    // Row 2: nothing selected (end is "before col 0")
    assert!(
        !sel.contains(2, 0),
        "col 0 on end row must NOT be selected when end side is Left at col 0"
    );
    assert!(!sel.contains(2, 1));

    // Row 3: not selected
    assert!(!sel.contains(3, 0));
}

/// Single-row selection where end is at col 0 with Left side should be empty.
#[test]
fn test_contains_end_left_side_col_zero_single_row() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 0, SelectionSide::Left);
    sel.complete_selection();

    // Selection start == end, should be empty.
    assert!(!sel.contains(0, 0));
}

/// Selection starting at col 0 with Left side should include col 0.
#[test]
fn test_contains_start_left_side_col_zero() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 5, SelectionSide::Right);
    sel.complete_selection();

    assert!(
        sel.contains(0, 0),
        "col 0 must be selected when start is Left-sided at col 0"
    );
    assert!(sel.contains(0, 3));
    assert!(sel.contains(0, 5));
    assert!(!sel.contains(0, 6));
}

/// project_range must agree with contains when end is Left-sided at col 0.
#[test]
fn test_project_range_end_left_side_col_zero() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 0, SelectionSide::Left);
    sel.complete_selection();

    let proj = sel.project_range(79).expect("should project");
    assert_eq!(proj.start_row, 0);
    assert_eq!(proj.start_col, 5);
    // End should retreat to previous row since Left at col 0 means
    // "before col 0" on row 2.
    assert_eq!(
        proj.end_row, 1,
        "end_row should retreat to row 1 when end is Left-sided at col 0"
    );
}

/// project_range returns None for single-row selection ending Left at col 0.
#[test]
fn test_project_range_end_left_side_col_zero_single_row_empty() {
    // Start at col 0 Left, end at col 0 Left => same anchor => empty.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 0, SelectionSide::Left);

    assert_eq!(
        sel.project_range(79),
        None,
        "single-cell same-anchor selection should project as empty"
    );
}

/// side_adjusted_bounds must retreat end_row when end is Left-sided at col 0.
#[test]
fn test_side_adjusted_bounds_end_left_col_zero() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(2, 0, SelectionSide::Left);
    sel.complete_selection();

    let bounds = sel
        .side_adjusted_bounds()
        .expect("should have side-adjusted bounds");
    assert_eq!(bounds.0, 0, "start_row");
    assert_eq!(bounds.1, 5, "start_col");
    assert_eq!(
        bounds.2, 1,
        "end_row should retreat to 1 when end is Left at col 0"
    );
}

/// Block selection: normalized_end always uses Right side, so user-provided
/// Left side on the end anchor does not cause a col 0 off-by-one for blocks.
/// The block normalization takes min/max of columns and forces Left/Right
/// sides, making the block from col 0 to col 0 (1-column wide).
#[test]
fn test_contains_block_end_left_side_col_zero() {
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 0, SelectionSide::Left);
    sel.complete_selection();

    // Block normalization forces start=Left, end=Right on the min/max columns.
    // Both anchors have col 0, so the block is 1 column wide at col 0.
    assert!(
        sel.contains(1, 0),
        "block selection col 0..=0 should include col 0 (normalized_end forces Right side)"
    );
    assert!(
        !sel.contains(1, 1),
        "col 1 should not be in a col 0..=0 block"
    );
}

#[test]
fn test_adjust_for_rows_shrink_relabels_every_row_by_the_same_amount() {
    // 10 visible rows shrinking to 6. Since the rows-only shapes anchor at the
    // CURSOR, the viewport gives up its top four rows to history and EVERY row —
    // live or already-archived — moves by exactly -4. Measured against the engine
    // in `probe`-style traces: rel 0 (line-55) -> rel -4, rel 7 (line-62) -> rel 3.
    let mut live = TextSelection::new();
    live.start_selection(2, 1, SelectionSide::Left, SelectionType::Simple);
    live.update_selection(5, 7, SelectionSide::Right);
    live.complete_selection();
    assert!(live.adjust_for_rows_shrink(6, 4, 50));
    assert_eq!(
        live.start().row,
        -2,
        "a live row moves by the rows given up"
    );
    assert_eq!(live.end().row, 1);

    let mut bottom = TextSelection::new();
    bottom.start_selection(6, 0, SelectionSide::Left, SelectionType::Simple);
    bottom.update_selection(9, 3, SelectionSide::Right);
    bottom.complete_selection();
    assert!(bottom.adjust_for_rows_shrink(6, 4, 50));
    assert_eq!(
        bottom.start().row,
        2,
        "the bottom rows stay live under the cursor"
    );
    assert_eq!(bottom.end().row, 5);
    assert_eq!(bottom.start().col, 0, "columns are untouched");
    assert!(bottom.is_complete(), "and so is the lifecycle state");

    let mut archived = TextSelection::new();
    archived.start_selection(-20, 0, SelectionSide::Left, SelectionType::Block);
    archived.update_selection(-7, 2, SelectionSide::Right);
    assert!(archived.adjust_for_rows_shrink(6, 4, 50));
    assert_eq!(archived.start().row, -24, "history gains four newer lines");
    assert_eq!(archived.end().row, -11);
    assert!(archived.is_in_progress());
    assert_eq!(archived.selection_type(), SelectionType::Block);

    // The SHAPE claim: one delta describes every regime at once. A live span and an
    // archived span move by the same 4 rows, which is what makes this a relabel and
    // not the piecewise map it used to be.
    let mut degenerate = TextSelection::new();
    degenerate.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
    degenerate.update_selection(1, 1, SelectionSide::Right);
    degenerate.complete_selection();
    assert!(!degenerate.adjust_for_rows_shrink(0, 4, 50));
    assert!(!degenerate.has_selection());

    // An empty selection is a no-op in every direction.
    let mut empty = TextSelection::new();
    assert!(empty.adjust_for_rows_shrink(6, 4, 50));
    assert!(!empty.has_selection());
}

/// A rows-only SHRINK is MONOTONIC, so no span can be torn by it.
///
/// This test is the negative of one that used to assert the opposite. The shrink
/// once demoted the bottom live rows into history, INSERTING them between the
/// existing scrollback and the rows that stayed on screen — a non-monotonic map,
/// under which a span crossing the cut remapped to a reversed, WIDER range. Since
/// anchors are a row RANGE rather than a set (the copy walk is
/// `first_row..=last_row`, and the normalized bounds take min/max independently),
/// such a span copied rows the user never selected, so the primitive cleared it.
///
/// Cursor anchoring makes the map a pure relabel, and a monotonic map cannot
/// reverse a span. The spans that had to be sacrificed now SURVIVE intact, which is
/// strictly better for the user: a height drag no longer destroys a highlight that
/// happens to cross the cut. Should the rows-only shapes ever stop anchoring at the
/// cursor, this test fails and the guard must come back with them.
#[test]
fn a_shrink_spares_a_span_that_crosses_the_cut() {
    // 10 visible rows -> 6. A span over live rows 4..=7 crosses the old cut.
    let mut sel = TextSelection::new();
    sel.start_selection(4, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(7, 5, SelectionSide::Right);
    sel.complete_selection();
    assert!(
        sel.adjust_for_rows_shrink(6, 4, 100),
        "the span survives the shrink"
    );
    assert_eq!(sel.start().row, 0, "…relabelled by the rows given up");
    assert_eq!(sel.end().row, 3);
    assert!(
        sel.start().row <= sel.end().row,
        "ORDER is the property the old map could not keep: a reversed range copies \
         rows the user never selected"
    );

    // …and the scrollback-into-live span, which the demoted rows used to split.
    let mut across = TextSelection::new();
    across.start_selection(-3, 0, SelectionSide::Left, SelectionType::Simple);
    across.update_selection(2, 5, SelectionSide::Right);
    across.complete_selection();
    assert!(
        across.adjust_for_rows_shrink(6, 4, 100),
        "a span from scrollback into the live rows is continuous under a relabel"
    );
    assert_eq!(across.start().row, -7);
    assert_eq!(across.end().row, -2);
    assert!(across.start().row <= across.end().row, "order again");
}
