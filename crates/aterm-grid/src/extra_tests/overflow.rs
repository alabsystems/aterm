// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

// =============================================================================
// u16 overflow boundary tests for shift_rows_up_by / shift_region_up_by
// =============================================================================

#[test]
fn shift_rows_up_by_u16_overflow_drops_all() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(u16::MAX - 1, 0))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(u16::MAX, 0))
        .add_combining('\u{0303}');
    assert_eq!(extras.len(), 3);

    extras.shift_rows_up_by(u16::MAX - 1, 3);

    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row below start_row should survive overflow shift"
    );
    assert!(
        extras.get(CellCoord::new(u16::MAX - 1, 0)).is_none(),
        "row at start_row should be dropped on overflow"
    );
    assert!(
        extras.get(CellCoord::new(u16::MAX, 0)).is_none(),
        "row above start_row should be dropped on overflow"
    );
    assert_eq!(extras.len(), 1, "only rows below start_row survive");
}

#[test]
fn shift_rows_up_by_exact_u16_max_boundary() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(0, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(u16::MAX - 1, 0))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(u16::MAX, 0))
        .add_combining('\u{0303}');

    extras.shift_rows_up_by(u16::MAX - 1, 1);

    assert!(
        extras.get(CellCoord::new(0, 0)).is_some(),
        "row 0 preserved"
    );
    let shifted = extras
        .get(CellCoord::new(u16::MAX - 1, 0))
        .expect("shifted row 65535 should land at 65534");
    assert_eq!(
        shifted.combining(),
        &['\u{0303}'],
        "shifted row should carry old row 65535 content"
    );
    assert!(
        extras.get(CellCoord::new(u16::MAX, 0)).is_none(),
        "old row 65535 should no longer exist at its original position"
    );
    assert_eq!(extras.len(), 2, "row 0 + shifted row");
}

#[test]
fn shift_region_up_by_u16_overflow_drops_region() {
    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(10, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(u16::MAX - 1, 0))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(u16::MAX, 0))
        .add_combining('\u{0303}');

    extras.shift_region_up_by(u16::MAX - 1, u16::MAX, 3);

    assert!(
        extras.get(CellCoord::new(10, 0)).is_some(),
        "row outside region should survive"
    );
    assert!(
        extras.get(CellCoord::new(u16::MAX - 1, 0)).is_none(),
        "region row at top should be dropped on overflow"
    );
    assert!(
        extras.get(CellCoord::new(u16::MAX, 0)).is_none(),
        "region row at bottom should be dropped on overflow"
    );
    assert_eq!(extras.len(), 1);
}
