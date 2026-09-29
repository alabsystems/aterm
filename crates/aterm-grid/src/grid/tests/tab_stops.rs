// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tab stop tests — default stops, set/clear, forward/back tab, resize preservation.

use super::super::*;

#[test]
fn grid_default_tab_stops() {
    let grid = Grid::new(24, 80);
    // Default tab stops are at columns 8, 16, 24, 32, 40, 48, 56, 64, 72
    // Column 0 should not be a tab stop
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
    // Column 5 is not a default tab stop
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
    // Column 8 is a default tab stop
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
    // Verify some default tab stops exist
    assert!(grid.storage.tab_stops[8]);
    assert!(grid.storage.tab_stops[16]);

    grid.clear_all_tab_stops();

    // All tab stops should be cleared
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

    // Default tab stops should be restored
    assert!(grid.storage.tab_stops[8]);
    assert!(grid.storage.tab_stops[16]);
}

#[test]
fn grid_tab_uses_custom_stops() {
    let mut grid = Grid::new(24, 80);
    // Clear all and set custom tab stops
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 12);
    grid.set_tab_stop();

    // Tab from column 0 should go to column 5
    grid.set_cursor(0, 0);
    grid.tab();
    assert_eq!(grid.cursor_col(), 5);

    // Tab from column 5 should go to column 12
    grid.tab();
    assert_eq!(grid.cursor_col(), 12);

    // Tab from column 12 should go to last column (no more stops)
    grid.tab();
    assert_eq!(grid.cursor_col(), 79);
}

#[test]
fn grid_back_tab_with_default_stops() {
    let mut grid = Grid::new(24, 80);
    // Start at column 20
    grid.set_cursor(0, 20);

    // Back tab should go to column 16 (previous default tab stop)
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 16);

    // Back tab should go to column 8
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 8);

    // Back tab should go to column 0 (no stop before 8)
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 0);

    // Back tab at column 0 should stay at 0
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_back_tab_with_custom_stops() {
    let mut grid = Grid::new(24, 80);
    // Clear all and set custom tab stops
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 12);
    grid.set_tab_stop();
    grid.set_cursor(0, 25);
    grid.set_tab_stop();

    // Start at column 30
    grid.set_cursor(0, 30);

    // Back tab should go to column 25
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 25);

    // Back tab should go to column 12
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 12);

    // Back tab should go to column 5
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 5);

    // Back tab should go to column 0 (no stop before 5)
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_back_tab_n() {
    let mut grid = Grid::new(24, 80);
    // Start at column 40
    grid.set_cursor(0, 40);

    // Back tab by 3 stops: 40 -> 32 -> 24 -> 16
    grid.back_tab_n(3);
    assert_eq!(grid.cursor_col(), 16);

    // Back tab by 10 stops (more than available): should go to column 0
    grid.back_tab_n(10);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn grid_back_tab_between_stops() {
    let mut grid = Grid::new(24, 80);
    // Start at column 10 (between tab stops 8 and 16)
    grid.set_cursor(0, 10);

    // Back tab should go to column 8
    grid.back_tab();
    assert_eq!(grid.cursor_col(), 8);
}

#[test]
fn grid_tab_n_with_default_stops() {
    let mut grid = Grid::new(24, 80);
    // Start at column 0
    grid.set_cursor(0, 0);

    // Tab forward by 3 stops: 0 -> 8 -> 16 -> 24
    grid.tab_n(3);
    assert_eq!(grid.cursor_col(), 24);

    // Tab forward by 1: 24 -> 32
    grid.tab_n(1);
    assert_eq!(grid.cursor_col(), 32);
}

#[test]
fn grid_tab_n_past_last_stop() {
    let mut grid = Grid::new(24, 80);
    // Start at column 0
    grid.set_cursor(0, 0);

    // Tab forward by 20 stops (more than available): should go to last column (79)
    grid.tab_n(20);
    assert_eq!(grid.cursor_col(), 79);
}

#[test]
fn grid_tab_n_with_custom_stops() {
    let mut grid = Grid::new(24, 80);
    // Clear all and set custom tab stops at columns 5, 15, 30
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 15);
    grid.set_tab_stop();
    grid.set_cursor(0, 30);
    grid.set_tab_stop();

    // Start at column 0
    grid.set_cursor(0, 0);

    // Tab forward by 2: 0 -> 5 -> 15
    grid.tab_n(2);
    assert_eq!(grid.cursor_col(), 15);

    // Tab forward by 1: 15 -> 30
    grid.tab_n(1);
    assert_eq!(grid.cursor_col(), 30);
}

#[test]
fn grid_tab_n_from_between_stops() {
    let mut grid = Grid::new(24, 80);
    // Start at column 10 (between default tab stops 8 and 16)
    grid.set_cursor(0, 10);

    // Tab forward by 2: 10 -> 16 -> 24
    grid.tab_n(2);
    assert_eq!(grid.cursor_col(), 24);
}

#[test]
fn grid_resize_preserves_tab_stops() {
    let mut grid = Grid::new(24, 40);
    // Clear defaults and set custom tab stop at column 5
    grid.clear_all_tab_stops();
    grid.set_cursor(0, 5);
    grid.set_tab_stop();

    // Resize to larger width
    grid.resize(24, 80);

    // Custom tab stop should be preserved
    assert!(
        grid.storage.tab_stops[5],
        "Custom tab stop at column 5 should be preserved"
    );
    // ...AND THE WIDEN INVENTS NONE. TBC 3 said "no stop at any column"; a
    // window getting wider is not an application asking for stops back. xterm
    // clears its whole (fixed-size) tab bitmap on TBC 3 and `ScreenResize`
    // never re-seeds it — the resurrection here was an artifact of aterm
    // materializing the array lazily at the current width.
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
    // The other half of the rule: a grid whose default every-8 stops are intact
    // must GAIN them in the columns a widen adds, or a tab past the old width
    // would run to the right edge. (This is what xterm gets for free from a
    // tab bitmap that is 1024 columns wide from the start.)
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
    // The backing array outlives the shrink and still covers column 96. (TBC 3
    // widens it to MAX_GRID_COLS so a later grow cannot re-seed cleared stops,
    // so this is a lower bound, not an equality.)
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

    // Negative control: the pre-`46fc93f5a` restore body, replayed on a real
    // destination as narrow as the model's (`cols == Narrow`, 40 real columns)
    // beside the shipping restore on an identical one. The old body admitted a
    // vector of any length and copied whatever prefix overlapped the
    // destination's own stops; the shipping one refuses both invalid sizes.
    // Projected onto the admission fields — did the destination take the
    // vector, and did it keep its own width (`restored_len == cols`) — the
    // pre-fix grid is exactly the model's `Buggy=1` admission, which the
    // covering/bounded window rejects.
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

#[test]
fn grid_is_tab_stop() {
    let mut grid = Grid::new(24, 80);

    // Default tab stops at columns 8, 16, 24, etc.
    assert!(!grid.storage.is_tab_stop(0)); // Column 0 is never a tab stop
    assert!(!grid.storage.is_tab_stop(1));
    assert!(grid.storage.is_tab_stop(8));
    assert!(grid.storage.is_tab_stop(16));
    assert!(!grid.storage.is_tab_stop(10));

    // Clear all and set custom
    grid.clear_all_tab_stops();
    assert!(!grid.storage.is_tab_stop(8));

    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    assert!(grid.storage.is_tab_stop(5));

    // Out of bounds returns false
    assert!(!grid.storage.is_tab_stop(1000));
}

#[test]
fn grid_tab_stop_positions() {
    let mut grid = Grid::new(24, 80);

    // Default tab stops: 8, 16, 24, 32, 40, 48, 56, 64, 72
    let positions: Vec<u16> = grid.storage.tab_stop_positions().collect();
    assert_eq!(positions, vec![8, 16, 24, 32, 40, 48, 56, 64, 72]);

    // Clear all and set custom stops
    grid.clear_all_tab_stops();
    let positions: Vec<u16> = grid.storage.tab_stop_positions().collect();
    assert!(positions.is_empty());

    // Set custom tab stops at 5, 10, 20
    grid.set_cursor(0, 5);
    grid.set_tab_stop();
    grid.set_cursor(0, 10);
    grid.set_tab_stop();
    grid.set_cursor(0, 20);
    grid.set_tab_stop();

    let positions: Vec<u16> = grid.storage.tab_stop_positions().collect();
    assert_eq!(positions, vec![5, 10, 20]);
}

// ===========================================================================
// CBT and the DECSLRM left margin — gated on ORIGIN MODE, not on DECLRMM
// ===========================================================================
//
// VT510's CBT page names no margin at all: "If an attempt is made to move the
// active position past the first character position on the line, then the
// active position stays at column one." DECOM is the page that turns "column
// one" into "the left margin": set, "the cursor cannot move outside of the
// margins"; reset, "the cursor can move outside of the margins".
//
// xterm composes them exactly so — tabs.c `TabToPrevStop` clamps to
// `ScrnLeftMargin` only inside `if (xw->flags & ORIGIN)` — and Ghostty says it
// in a comment: `// With origin mode enabled, our leftmost limit is the left
// margin.` (`horizontalTabBack`). `ScrnLeftMargin` is 0 unless DECLRMM is on,
// so both flags must be set for the margin to bind.
//
// aterm used to gate on DECLRMM alone, justified in-code as "matching CUB
// behavior". That is the inference both xterm and Ghostty declined to make:
// each clamps CUB on the left margin unconditionally and still leaves CBT
// origin-gated, in the same file.

/// A 3x40 grid with DECSLRM margins at columns 10..=30 and the default tab
/// stops (every 8: 8, 16, 24, 32).
fn grid_with_margins_at_10_30() -> Grid {
    let mut grid = Grid::new(3, 40);
    grid.set_horizontal_margins(10, 30);
    assert!(grid.has_horizontal_margins());
    assert!(grid.tab_stops()[8], "the stop at 8 is left of the margin");
    grid
}

/// DECLRMM alone does NOT make the left margin bind: without origin mode the
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

/// A cursor ALREADY LEFT of the left margin is not dragged rightwards onto it.
///
/// xterm does drag it (`if (next_column < left) next_column = left;` then
/// `set_cur_col`), which makes a BACKWARD tab move the cursor FORWARD. Ghostty
/// structurally cannot — its bound is the loop guard `if (x <= left_limit)
/// return;` — and pins the case in `test "Terminal: horizontal tab back with
/// cursor before left margin"`. CBT is backward motion by definition, so the
/// cursor stands still. (aterm used to send it to column 0 here, i.e. FURTHER
/// outside the margin the active DECOM says it may not leave.)
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

/// CHT is deliberately NOT changed to match xterm here.
///
/// xterm's `TabToNextStop` (tabs.c) computes the destination and then clamps it
/// DOWN to `rgt_marg`, so a CHT with the cursor already right of the right
/// margin moves the cursor BACKWARD — a forward tab that goes backwards. It is
/// a defect, not a convention: xterm's own `CursorForward` (cursor.c) handles
/// the identical situation with `if (screen->cur_col > max) max = screen->max_col;`,
/// the escape `TabToNextStop` lacks, and `CursorBack` states the rule in
/// English ("if the cursor is already before the left-margin, we have to let it
/// go"). Ghostty avoids it structurally (`while (cursor.x < scrolling_region.right)`)
/// and iTerm2 patched it explicitly. aterm's `tab_margin` uses the row edge when
/// the cursor is outside the margins, so it never moved backward either — this
/// test is the pin that keeps it that way.
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
