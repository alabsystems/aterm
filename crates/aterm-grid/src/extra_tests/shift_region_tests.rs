// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for CellExtras shift_region and shift_rows boundary operations.

use super::*;

// =============================================================================
// shift_region_up_by
// =============================================================================

#[test]
fn cell_extras_shift_region_up_by_basic() {
    let mut extras = CellExtras::new();

    for row in 0..5u16 {
        let mark = char::from_u32(0x0301 + u32::from(row)).expect("invariant: valid codepoint");
        extras
            .get_or_create(CellCoord::new(row, 0))
            .add_combining(mark);
    }
    assert_eq!(extras.len(), 5);

    extras.shift_region_up_by(1, 3, 1);

    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row 0 preserved"
    );

    let r1 = extras
        .get(CellCoord::new(1, 0))
        .expect("row 1 should exist after shift");
    let expected_mark = char::from_u32(0x0303).expect("invariant: valid codepoint");
    assert_eq!(r1.combining(), &[expected_mark], "row 2 shifted to row 1");

    let r2 = extras
        .get(CellCoord::new(2, 0))
        .expect("row 2 should exist after shift");
    let expected_mark = char::from_u32(0x0304).expect("invariant: valid codepoint");
    assert_eq!(r2.combining(), &[expected_mark], "row 3 shifted to row 2");

    assert!(
        extras.get(CellCoord::new(4, 0)).is_some(),
        "row 4 preserved"
    );
}

#[test]
fn cell_extras_shift_region_up_by_zero_is_noop() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0301}');

    extras.shift_region_up_by(1, 3, 0);
    assert_eq!(extras.len(), 1, "shift_region_up_by with n=0 is no-op");
    assert!(extras.get(CellCoord::new(2, 0)).is_some());
}

#[test]
fn cell_extras_shift_region_up_by_deletes_top_row_explicitly() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(1, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0302}');

    extras.shift_region_up_by(1, 3, 1);

    let shifted = extras
        .get(CellCoord::new(1, 0))
        .expect("row 2 should shift into row 1");
    assert_eq!(shifted.combining(), &['\u{0302}']);
    assert_eq!(extras.len(), 1, "top-row extras should be deleted");
}

// =============================================================================
// shift_region_down_by
// =============================================================================

#[test]
fn cell_extras_shift_region_down_by_basic() {
    let mut extras = CellExtras::new();

    for row in 0..5u16 {
        let mark = char::from_u32(0x0301 + u32::from(row)).expect("invariant: valid codepoint");
        extras
            .get_or_create(CellCoord::new(row, 0))
            .add_combining(mark);
    }

    extras.shift_region_down_by(1, 3, 1);

    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row 0 preserved"
    );

    let r2 = extras
        .get(CellCoord::new(2, 0))
        .expect("row 2 should exist after shift");
    let expected_mark = char::from_u32(0x0302).expect("invariant: valid codepoint");
    assert_eq!(r2.combining(), &[expected_mark], "row 1 shifted to row 2");

    let r3 = extras
        .get(CellCoord::new(3, 0))
        .expect("row 3 should exist after shift");
    let expected_mark = char::from_u32(0x0303).expect("invariant: valid codepoint");
    assert_eq!(r3.combining(), &[expected_mark], "row 2 shifted to row 3");

    assert!(
        extras.get(CellCoord::new(4, 0)).is_some(),
        "row 4 preserved"
    );
}

#[test]
fn cell_extras_shift_region_down_by_zero_is_noop() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0301}');

    extras.shift_region_down_by(1, 3, 0);
    assert_eq!(extras.len(), 1, "shift_region_down_by with n=0 is no-op");
    assert!(extras.get(CellCoord::new(2, 0)).is_some());
}

// =============================================================================
// shift_rows_up_by boundary conditions
// =============================================================================

#[test]
fn cell_extras_shift_rows_up_by_boundary_u16_max() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(u16::MAX, 0))
        .add_combining('\u{0301}');

    extras.shift_rows_up_by(0, 1);

    assert!(
        extras.get(CellCoord::new(u16::MAX, 0)).is_none(),
        "old position should be gone"
    );
    assert!(
        extras.get(CellCoord::new(u16::MAX - 1, 0)).is_some(),
        "should have shifted to u16::MAX - 1"
    );
}

#[test]
fn cell_extras_shift_rows_up_by_overflow_drops_all() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(5, 0))
        .add_combining('\u{0302}');

    extras.shift_rows_up_by(1, u16::MAX);

    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row 0 preserved"
    );
    assert!(extras.get(CellCoord::new(5, 0)).is_none(), "row 5 dropped");
    assert_eq!(extras.len(), 1, "only row 0 survives overflow");
}

// =============================================================================
// Algorithm audit boundary cases (#4335)
// =============================================================================

#[test]
fn shift_region_up_single_row_region_deletes_only_top() {
    // Region where top == bottom: single-row region.
    // shift_region_up_by(5, 5, 1) should delete row 5 only.
    let mut extras = CellExtras::new();
    for row in 4..=6u16 {
        extras
            .get_or_create(CellCoord::new(row, 0))
            .add_combining('\u{0301}');
    }
    assert_eq!(extras.len(), 3);

    extras.shift_region_up_by(5, 5, 1);

    assert!(
        extras.get(CellCoord::new(4, 0)).is_some(),
        "row 4 preserved"
    );
    assert!(extras.get(CellCoord::new(5, 0)).is_none(), "row 5 deleted");
    assert!(
        extras.get(CellCoord::new(6, 0)).is_some(),
        "row 6 preserved"
    );
    assert_eq!(extras.len(), 2);
}

#[test]
fn shift_region_up_n_equals_region_size_deletes_all() {
    // Region [2, 5] with n=4: all 4 rows in region should be deleted.
    let mut extras = CellExtras::new();
    for row in 0..8u16 {
        extras
            .get_or_create(CellCoord::new(row, 0))
            .add_combining('\u{0301}');
    }
    assert_eq!(extras.len(), 8);

    // n == (bottom - top + 1) = 4: entire region deleted.
    extras.shift_region_up_by(2, 5, 4);

    // Rows 0, 1 (before region) and 6, 7 (after region) survive.
    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row 0 preserved"
    );
    assert!(
        extras.get(CellCoord::new(1, 0)).is_some(),
        "row 1 preserved"
    );
    assert!(extras.get(CellCoord::new(2, 0)).is_none(), "row 2 deleted");
    assert!(extras.get(CellCoord::new(3, 0)).is_none(), "row 3 deleted");
    assert!(extras.get(CellCoord::new(4, 0)).is_none(), "row 4 deleted");
    assert!(extras.get(CellCoord::new(5, 0)).is_none(), "row 5 deleted");
    assert!(
        extras.get(CellCoord::new(6, 0)).is_some(),
        "row 6 preserved"
    );
    assert!(
        extras.get(CellCoord::new(7, 0)).is_some(),
        "row 7 preserved"
    );
    assert_eq!(extras.len(), 4);
}

#[test]
fn shift_region_down_n_exceeds_region_drops_all_in_region() {
    // Region [1, 3] with n=5: all rows shifted beyond bottom are dropped.
    let mut extras = CellExtras::new();
    for row in 0..5u16 {
        extras
            .get_or_create(CellCoord::new(row, 0))
            .add_combining('\u{0301}');
    }

    extras.shift_region_down_by(1, 3, 5);

    // Row 0 preserved (below region), row 4 preserved (above region).
    // Rows 1-3 shifted by 5 → rows 6-8, but drop_start = 3 - (5-1) = 3-4 → saturates to 0.
    // All rows >= 0 inside [1,3] are in drop zone, so all region rows dropped.
    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row 0 preserved"
    );
    assert!(
        extras.get(CellCoord::new(4, 0)).is_some(),
        "row 4 preserved"
    );
    assert_eq!(extras.len(), 2, "region rows all dropped");
}
