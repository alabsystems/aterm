// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Scaling tests for CellExtras operations, using deterministic op counters.
//! A full-screen `shift_rows_up_by(0, n)` is an O(1) offset bump, with one O(E)
//! compaction once the offset reaches `COMPACT_THRESHOLD` (256).

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

    extras.shift_rows_up_by(0, 1);
    let ops = take_extras_shift_ops();
    assert_eq!(
        ops, 0,
        "full-screen shift should be O(1) via offset bump, got {ops} ops"
    );

    // Old row 1, read through the offset, is now row 0.
    assert!(extras.get(CellCoord::new(0, 0)).is_some());
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

    // The counter records the map size at call time.
    assert_eq!(ops, 500, "clear_row should scan all entries (got {ops})");
}

/// Full-screen scrolls below the compaction threshold cost 0 shift ops.
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
    let scroll_count = 50;
    for _ in 0..scroll_count {
        extras.shift_rows_up_by(0, 1);
    }
    let total_ops = take_extras_shift_ops();

    assert_eq!(
        total_ops, 0,
        "50 scrolls below compact threshold should cost 0 shift ops, got {total_ops}"
    );

    assert_eq!(extras.row_offset(), 50);

    assert_eq!(extras.len(), 0, "all entries should have scrolled off");
}

/// The scroll that brings the offset to the threshold compacts, at O(E).
#[test]
fn compaction_at_threshold_costs_o_e() {
    use crate::test_counters::take_extras_shift_ops;
    let _ = take_extras_shift_ops(); // reset

    // Row 500 outlives 256 scrolls.
    let mut extras = CellExtras::new();
    for col in 0..100u16 {
        extras
            .get_or_create(CellCoord::new(500, col))
            .set_fg_rgb(Some([255, 0, 0]));
    }
    let _ = take_extras_shift_ops(); // reset

    for _ in 0..255 {
        extras.shift_rows_up_by(0, 1);
    }
    let ops_before_threshold = take_extras_shift_ops();
    assert_eq!(ops_before_threshold, 0, "255 scrolls should be O(1)");

    extras.shift_rows_up_by(0, 1);
    let ops_at_threshold = take_extras_shift_ops();
    assert!(
        ops_at_threshold > 0,
        "256th scroll should trigger O(E) compaction, got {ops_at_threshold}"
    );
    assert_eq!(
        extras.row_offset(),
        0,
        "offset should reset after compaction"
    );
}
