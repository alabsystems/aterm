// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

// u16 overflow boundary test for `shift_region_up_by`

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
