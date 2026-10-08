// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scaling tests for CellExtras operations, using deterministic op counters.
//!
//! `shift_rows_up_by(0, n)` bumps a row offset in O(1) for full-screen scrolls,
//! with one O(E) compaction every COMPACT_THRESHOLD (256) scrolls.

use super::*;

/// `shift_rows_up_by(0, n)` registers 0 shift ops (an offset bump).
#[test]
fn shift_rows_up_by_full_screen_is_o1() {
    use crate::test_counters::take_extras_shift_ops;
    let _ = take_extras_shift_ops(); // reset

    let mut extras = CellExtras::new();
    for row in 0..10u16 {
        for col in 0..50u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_fg_rgb(Some([255, 0, 0]));
        }
    }
    let _ = take_extras_shift_ops(); // reset counter

    // Full-screen shift: O(1) offset bump, 0 shift ops
    extras.shift_rows_up_by(0, 1);
    let ops = take_extras_shift_ops();
    assert_eq!(
        ops, 0,
        "full-screen shift should be O(1) via offset bump, got {ops} ops"
    );

    // Verify data is still accessible via translated coords
    assert!(extras.get(CellCoord::new(0, 0)).is_some());
    // Row 0 was at row 1 before the shift — now shifted up to row 0.
    // Original row 0 scrolled off and is stale.
}

/// `shift_rows_up_by(start > 0, n)` is O(E): only some rows shift, so no
/// offset amortization.
#[test]
fn shift_rows_up_by_partial_is_o_e() {
    use crate::test_counters::take_extras_shift_ops;
    let _ = take_extras_shift_ops(); // reset

    let mut extras = CellExtras::new();
    for row in 0..10u16 {
        for col in 0..50u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_fg_rgb(Some([0, 255, 0]));
        }
    }
    let _ = take_extras_shift_ops(); // reset counter

    // Partial shift (start_row > 0): compacts + drain-rebuilds = O(E)
    extras.shift_rows_up_by(5, 1);
    let ops = take_extras_shift_ops();
    assert!(
        ops > 0,
        "partial shift (start_row > 0) should use O(E) drain-rebuild, got {ops}"
    );
}

/// `clear_row` is O(E): `retain()` scans every entry whichever row is cleared.
#[test]
fn clear_row_cost_linear_in_total_extras() {
    use crate::test_counters::take_extras_clear_ops;
    let _ = take_extras_clear_ops(); // reset

    // Build extras across many rows
    let mut extras = CellExtras::new();
    for row in 0..20u16 {
        for col in 0..25u16 {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_fg_rgb(Some([128, 128, 128]));
        }
    }
    assert_eq!(extras.len(), 500);

    let _ = take_extras_clear_ops(); // reset
    extras.clear_row(10); // Clear one row (25 entries)
    let ops = take_extras_clear_ops();

    // retain() scans all 500 entries to remove 25 for row 10.
    // The counter records the map size at call time, so ops is exactly 500.
    assert_eq!(ops, 500, "clear_row should scan all entries (got {ops})");
}

/// Full-screen scrolls cost 0 shift ops below the threshold and one O(E)
/// compaction at it: amortized O(E / COMPACT_THRESHOLD) per scroll.
#[test]
fn scroll_compound_cost_amortized_o1() {
    use crate::test_counters::take_extras_shift_ops;
    let _ = take_extras_shift_ops(); // reset

    let mut extras = CellExtras::new();
    let extras_per_row = 50u16;
    let num_rows = 10u16;
    for row in 0..num_rows {
        for col in 0..extras_per_row {
            extras
                .get_or_create(CellCoord::new(row, col))
                .set_fg_rgb(Some([255, 128, 0]));
        }
    }
    let initial_e = extras.len();
    assert_eq!(initial_e, (num_rows * extras_per_row) as usize);

    let _ = take_extras_shift_ops(); // reset
    // 50 scrolls, all below COMPACT_THRESHOLD (256) → 0 shift ops
    let scroll_count = 50;
    for _ in 0..scroll_count {
        extras.shift_rows_up_by(0, 1);
    }
    let total_ops = take_extras_shift_ops();

    // All 50 scrolls used O(1) offset bumps — 0 shift ops total.
    // This is the key improvement over the former O(N*E) = 2750 ops.
    assert_eq!(
        total_ops, 0,
        "50 scrolls below compact threshold should cost 0 shift ops, got {total_ops}"
    );

    // Verify offset accumulated correctly
    assert_eq!(extras.row_offset(), 50);

    // After all rows scrolled off (10 rows, 50 shifts), no valid entries remain
    assert_eq!(extras.len(), 0, "all entries should have scrolled off");
}

/// Prove that compaction happens at the threshold and costs O(E).
#[test]
fn compaction_at_threshold_costs_o_e() {
    use crate::test_counters::take_extras_shift_ops;
    let _ = take_extras_shift_ops(); // reset

    let mut extras = CellExtras::new();
    // Create entries at high rows so they survive many scrolls
    for col in 0..100u16 {
        extras
            .get_or_create(CellCoord::new(500, col))
            .set_fg_rgb(Some([255, 0, 0]));
    }
    let _ = take_extras_shift_ops(); // reset

    // Scroll 255 times → all O(1) offset bumps, 0 ops
    for _ in 0..255 {
        extras.shift_rows_up_by(0, 1);
    }
    let ops_before_threshold = take_extras_shift_ops();
    assert_eq!(ops_before_threshold, 0, "255 scrolls should be O(1)");

    // 256th scroll triggers compaction (offset reaches COMPACT_THRESHOLD)
    extras.shift_rows_up_by(0, 1);
    let ops_at_threshold = take_extras_shift_ops();
    assert!(
        ops_at_threshold > 0,
        "256th scroll should trigger O(E) compaction, got {ops_at_threshold}"
    );
    // After compaction, offset resets to 0
    assert_eq!(
        extras.row_offset(),
        0,
        "offset should reset after compaction"
    );
}
