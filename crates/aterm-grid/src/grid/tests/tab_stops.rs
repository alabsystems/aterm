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
    // New default tab stops should be added for new columns
    assert!(
        grid.storage.tab_stops[48],
        "Default tab stop at column 48 should be added"
    );
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
    assert_eq!(source.tab_stops().len(), 120);
    assert!(source.tab_stops()[96]);

    let carried = source.tab_stops().to_vec();
    assert!(model.fire("CaptureProjection", &mut state));
    let mut restored = Grid::new(6, 40);
    restored.restore_tab_stops(&carried);
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
    restored.restore_tab_stops(&[false; 119]);
    let undersize = model.successors("SupplyUndersizeProjection", &model.init_state())[0].clone();
    let undersize_rejected = model.successors("RejectUndersizeProjection", &undersize)[0].clone();
    assert_eq!(restored.tab_stops(), before, "short projection is rejected");
    assert_eq!(undersize_rejected.get("rejected"), Some(&1));
    restored.restore_tab_stops(&vec![false; usize::from(crate::MAX_GRID_COLS) + 1]);
    let oversize = model.successors("SupplyOversizeProjection", &model.init_state())[0].clone();
    let oversize_rejected = model.successors("RejectOversizeProjection", &oversize)[0].clone();
    assert_eq!(
        restored.tab_stops(),
        before,
        "protocol-oversize projection is rejected"
    );
    assert_eq!(oversize_rejected.get("rejected"), Some(&1));
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
