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

    // Scrolling the start endpoint below the retained-history floor (its row
    // would drop to -41 - 460 = -501 < -500) means the content is truly
    // evicted, so the selection is correctly cleared.
    let visible = sel.adjust_for_scroll(460, max_rows, floor);
    assert!(
        !visible,
        "selection clears once an endpoint scrolls below the history floor"
    );
    assert!(!sel.has_selection());
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

    let mut evicted = TextSelection::new();
    evicted.start_selection(-5, 0, SelectionSide::Left, SelectionType::Simple);
    evicted.update_selection(-2, 2, SelectionSide::Right);
    evicted.complete_selection();
    assert!(!evicted.adjust_for_row_splice(3, 1, 5, 5));
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

#[test]
fn test_contains_cell_simple_selection_ignores_wide() {
    // Simple (linear) selection should not do wide-char snapping -- it works
    // at character level, not column-rectangle level.
    let mut sel = TextSelection::new();
    sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, 10, SelectionSide::Right);
    sel.complete_selection();

    // Wide char at col 4: outside selection, should NOT be included.
    assert!(
        !sel.contains_cell(0, 4, true, false),
        "simple selection should not snap wide chars at boundary"
    );
    // Regular cell at col 5: inside selection.
    assert!(sel.contains_cell(0, 5, false, false));
}

#[test]
fn test_contains_cell_block_continuation_at_col_zero() {
    // Edge case: continuation at column 0 (shouldn't happen in practice,
    // but must not underflow).
    let mut sel = TextSelection::new();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 5, SelectionSide::Right);
    sel.complete_selection();

    // is_wide_continuation at col 0: col > 0 is false, falls through to normal contains.
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
