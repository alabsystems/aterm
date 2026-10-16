// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tab stop tests — default stops, set/clear, forward/back tab, resize preservation.

use super::super::*;

#[test]
fn grid_default_tab_stops() {
    let grid = Grid::new(24, 80);
    let expected_tabs = [8, 16, 24, 32, 40, 48, 56, 64, 72];
    for col in &expected_tabs {
        assert!(
            grid.storage.tab_stops[*col],
            "Expected tab stop at column {col}"
        );
    }
    assert!(
        !grid.storage.tab_stops[0],
        "Column 0 should not be a tab stop"
    );
    assert!(
        !grid.storage.tab_stops[1],
        "Column 1 should not be a tab stop"
    );
}

#[test]
fn grid_set_tab_stop() {
    let mut grid = Grid::new(24, 80);
    assert!(!grid.storage.tab_stops[5]);

    grid.set_cursor(0, 5);
    grid.set_tab_stop();

    assert!(
        grid.storage.tab_stops[5],
        "Tab stop should be set at column 5"
    );
}

#[test]
fn grid_clear_tab_stop() {
    let mut grid = Grid::new(24, 80);
    assert!(grid.storage.tab_stops[8]);

    grid.set_cursor(0, 8);
    grid.clear_tab_stop();

    assert!(
        !grid.storage.tab_stops[8],
        "Tab stop should be cleared at column 8"
    );
}

#[test]
fn grid_clear_all_tab_stops() {
    let mut grid = Grid::new(24, 80);
    assert!(grid.storage.tab_stops[8]);
    assert!(grid.storage.tab_stops[16]);

    grid.clear_all_tab_stops();

    for col in 0..80 {
        assert!(
            !grid.storage.tab_stops[col],
            "Tab stop at column {col} should be cleared"
        );
    }
}

#[test]
fn grid_reset_tab_stops() {
    let mut grid = Grid::new(24, 80);
    grid.clear_all_tab_stops();
    assert!(!grid.storage.tab_stops[8]);

    grid.reset_tab_stops();

    assert!(grid.storage.tab_stops[8]);
    assert!(grid.storage.tab_stops[16]);
}

#[test]
fn grid_tab_uses_custom_stops() {
    let mut grid = Grid::new(24, 80);
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 12);
    grid.set_tab_stop();

    grid.set_cursor(0, 0);
    grid.tab();
    assert_eq!(grid.cursor_col(), 5);

    grid.tab();
    assert_eq!(grid.cursor_col(), 12);

    grid.tab();
    assert_eq!(grid.cursor_col(), 79);
}

#[test]
fn grid_back_tab_with_default_stops() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 20);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 16);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 8);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 0);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_back_tab_with_custom_stops() {
    let mut grid = Grid::new(24, 80);
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 12);
    grid.set_tab_stop();
    grid.set_cursor(0, 25);
    grid.set_tab_stop();

    grid.set_cursor(0, 30);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 25);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 12);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 5);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_back_tab_n() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 40);

    grid.back_tab_n(3);
    assert_eq!(grid.cursor_col(), 16);

    grid.back_tab_n(10);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_back_tab_between_stops() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 10);

    grid.back_tab();
    assert_eq!(grid.cursor_col(), 8);
}

#[test]
fn grid_tab_n_with_default_stops() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 0);

    grid.tab_n(3);
    assert_eq!(grid.cursor_col(), 24);

    grid.tab_n(1);
    assert_eq!(grid.cursor_col(), 32);
}

#[test]
fn grid_tab_n_past_last_stop() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 0);

    grid.tab_n(20);
    assert_eq!(grid.cursor_col(), 79);
}

#[test]
fn grid_tab_n_with_custom_stops() {
    let mut grid = Grid::new(24, 80);
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 15);
    grid.set_tab_stop();
    grid.set_cursor(0, 30);
    grid.set_tab_stop();

    grid.set_cursor(0, 0);

    grid.tab_n(2);
    assert_eq!(grid.cursor_col(), 15);

    grid.tab_n(1);
    assert_eq!(grid.cursor_col(), 30);
}

#[test]
fn grid_tab_n_from_between_stops() {
    let mut grid = Grid::new(24, 80);
    grid.set_cursor(0, 10);

    grid.tab_n(2);
    assert_eq!(grid.cursor_col(), 24);
}

#[test]
fn grid_resize_preserves_tab_stops() {
    let mut grid = Grid::new(24, 40);
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();

    grid.resize(24, 80);

    assert!(
        grid.storage.tab_stops[5],
        "Custom tab stop at column 5 should be preserved"
    );
    // And the widen invents none: TBC 3 means no stop at any column, and a
    // wider window is not a request for stops (xterm never re-seeds its bitmap).
    assert!(
        !grid.storage.tab_stops[48],
        "a widen must not resurrect a default stop the application cleared"
    );
    assert!(
        grid.storage.tab_stops.iter().filter(|&&s| s).count() == 1,
        "column 5 is the ONLY stop after TBC 3 + one HTS, at any width"
    );
}

#[test]
fn grid_widen_still_seeds_defaults_when_no_one_cleared_them() {
    // A grid whose default every-8 stops are intact gains them in the columns a
    // widen adds, so a tab past the old width does not run to the right edge.
    let mut grid = Grid::new(24, 40);
    grid.resize(24, 80);
    assert!(grid.storage.tab_stops[48], "default stop at 48 after widen");
    grid.set_cursor(0, 41);
    grid.tab();
    assert_eq!(grid.cursor_col(), 48, "HT reaches the seeded default stop");
}

#[test]
fn restore_preserves_bounded_stops_beyond_narrow_width_for_later_grow() {
    let model = aterm_spec::derive::tab_stop_handoff_model();
    let mut state = model.init_state();
    let mut source = Grid::new(6, 120);
    assert!(model.fire("GrowSourceWide", &mut state));
    source.clear_all_tab_stops();
    source.set_cursor(0, 96);
    source.set_tab_stop();
    assert!(model.fire("SetCustomFutureStop", &mut state));
    source.resize(6, 40);
    assert!(model.fire("ShrinkSourceNarrow", &mut state));
    // The backing array outlives the shrink and still covers column 96.
    assert!(source.tab_stops().len() >= 120);
    assert!(source.tab_stops()[96]);

    let carried = source.tab_stops().to_vec();
    assert!(model.fire("CaptureProjection", &mut state));
    let mut restored = Grid::new(6, 40);
    restored.restore_tab_stops(&carried, false);
    assert!(model.fire("AdmitCoveringProjection", &mut state));
    assert!(model.fire("RestoreProjection", &mut state));
    assert_eq!(restored.tab_stops(), carried);
    restored.resize(6, 120);
    assert!(model.fire("GrowDestinationWide", &mut state));
    restored.set_cursor(0, 0);
    restored.tab();
    assert!(model.fire("TabUsesRestoredStop", &mut state));
    assert_eq!(
        restored.cursor_col(),
        96,
        "a checkpoint shrink/grow keeps the custom off-width stop"
    );

    let before = restored.tab_stops().to_vec();
    restored.restore_tab_stops(&[false; 119], false);
    let undersize = model.successors("SupplyUndersizeProjection", &model.init_state())[0].clone();
    let undersize_rejected = model.successors("RejectUndersizeProjection", &undersize)[0].clone();
    assert_eq!(restored.tab_stops(), before, "short projection is rejected");
    assert_eq!(undersize_rejected.get("rejected"), Some(&1));
    restored.restore_tab_stops(&vec![false; usize::from(crate::MAX_GRID_COLS) + 1], false);
    let oversize = model.successors("SupplyOversizeProjection", &model.init_state())[0].clone();
    let oversize_rejected = model.successors("RejectOversizeProjection", &oversize)[0].clone();
    assert_eq!(
        restored.tab_stops(),
        before,
        "protocol-oversize projection is rejected"
    );
    assert_eq!(oversize_rejected.get("rejected"), Some(&1));

    // Negative control: a restore that admits a vector of any length and copies
    // the overlapping prefix, replayed on a 40-column destination beside the
    // shipping restore. Projected onto the admission fields (did it take the
    // vector, did it keep its width), it is the model's `Buggy=1` admission,
    // which the covering/bounded window rejects.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    for (stops, invalid, reject) in [
        (vec![false; 39], &undersize, "RejectUndersizeProjection"),
        (
            vec![false; usize::from(crate::MAX_GRID_COLS) + 1],
            &oversize,
            "RejectOversizeProjection",
        ),
    ] {
        let mut shipping = Grid::new(6, 40);
        let defaults = shipping.tab_stops().to_vec();
        shipping.restore_tab_stops(&stops, false);
        assert_eq!(shipping.tab_stops(), defaults, "{reject}: shipping refuses");

        let mut pre_fix = Grid::new(6, 40);
        let len = stops.len().min(pre_fix.storage.tab_stops.len());
        pre_fix.storage.tab_stops[..len].copy_from_slice(&stops[..len]);
        let took = pre_fix.tab_stops() != defaults;
        assert!(
            took,
            "{reject}: the pre-fix restore took the invalid vector"
        );
        let kept_width = pre_fix.tab_stops().len() == usize::from(pre_fix.cols());
        let mut projected = invalid.clone();
        projected.insert("phase", 10);
        projected.insert("admitted", i64::from(took));
        projected.insert("rejected", i64::from(!took));
        if kept_width {
            projected.insert("restored_len", invalid["cols"]);
        }
        assert_eq!(
            buggy.successors(reject, invalid),
            vec![projected.clone()],
            "{reject}"
        );
        assert!(!model.check_invariant("AdmissionIsCoveringAndBounded", &projected));
        assert!(
            model.successors(reject, invalid) != vec![projected],
            "{reject}"
        );
    }
}

// CBT and the DECSLRM left margin: gated on origin mode, not on DECLRMM.
//
// VT510's CBT stops at column one; DECOM turns "column one" into "the left
// margin". xterm's `TabToPrevStop` clamps to `ScrnLeftMargin` only under
// ORIGIN, and Ghostty's `horizontalTabBack` likewise. `ScrnLeftMargin` is 0
// unless DECLRMM is on, so both flags must be set for the margin to bind.

/// A 3x40 grid with DECSLRM margins at columns 10..=30 and the default tab
/// stops (every 8: 8, 16, 24, 32).
fn grid_with_margins_at_10_30() -> Grid {
    let mut grid = Grid::new(3, 40);
    grid.set_horizontal_margins(10, 30);
    assert!(grid.has_horizontal_margins());
    assert!(grid.tab_stops()[8], "the stop at 8 is left of the margin");
    grid
}

/// DECLRMM alone does not make the left margin bind: without origin mode the
/// back tab runs past it to the real stop at column 8.
#[test]
fn cbt_ignores_the_left_margin_without_origin_mode() {
    let mut grid = grid_with_margins_at_10_30();
    grid.set_cursor(0, 12);

    grid.back_tab_margin(true, false);

    assert_eq!(
        grid.cursor_col(),
        8,
        "with DECOM reset the cursor may leave the margins, so CBT reaches the \
         tab stop at 8 instead of stopping on the left margin at 10"
    );
}

/// With origin mode set the margin binds, exactly as xterm and Ghostty do it.
#[test]
fn cbt_clamps_to_the_left_margin_under_origin_mode() {
    let mut grid = grid_with_margins_at_10_30();
    grid.set_cursor(0, 12);

    grid.back_tab_margin(true, true);

    assert_eq!(
        grid.cursor_col(),
        10,
        "DECOM set: the cursor cannot move outside the margins, so CBT stops on \
         the left margin with no tab stop between 10 and 12"
    );
}

/// A cursor already left of the left margin is not dragged onto it: CBT is
/// backward motion, so the cursor stands still (as in Ghostty; xterm drags it
/// forward).
#[test]
fn cbt_left_of_the_left_margin_never_moves_the_cursor() {
    let mut grid = grid_with_margins_at_10_30();
    grid.set_cursor(0, 3);

    grid.back_tab_margin(true, true);

    assert_eq!(
        grid.cursor_col(),
        3,
        "a back tab from outside the left margin moves neither forward to the \
         margin (xterm) nor further back to column 0 (aterm's old rule)"
    );
}

/// With no margins armed at all, origin mode changes nothing: CBT is the plain
/// back tab, floored at column 0.
#[test]
fn cbt_without_declrmm_is_the_plain_back_tab_in_either_mode() {
    for origin in [false, true] {
        let mut grid = Grid::new(3, 40);
        grid.set_cursor(0, 5);
        grid.back_tab_margin(false, origin);
        assert_eq!(
            grid.cursor_col(),
            0,
            "origin={origin}: no stop below 5, so CBT floors at column 0"
        );
    }
}

/// CHT right of the right margin never moves backward. xterm's `TabToNextStop`
/// clamps the destination down to `rgt_marg` (its `CursorForward` does not, nor
/// do Ghostty and iTerm2); `tab_margin` uses the row edge outside the margins.
#[test]
fn cht_right_of_the_right_margin_never_moves_the_cursor_backward() {
    let mut grid = grid_with_margins_at_10_30();
    grid.set_cursor(0, 33);

    grid.tab_margin(true);

    assert!(
        grid.cursor_col() >= 33,
        "a FORWARD tab must never move the cursor backward; xterm's clamp-down \
         to the right margin would have landed it on 30, got {}",
        grid.cursor_col()
    );
}
