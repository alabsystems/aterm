// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{CellFlags, PackedColor};

#[test]
fn map_empty_row_with_ring_rgb_still_extracts() {
    let mut grid = Grid::new(3, 8);
    assert!(
        grid.row_mut(0).expect("row 0").write_char_styled(
            0,
            'X',
            PackedColor::rgb(9, 8, 7),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        ),
        "precondition: the styled write must land"
    );
    grid.extras_mut()
        .set_rgb_ring_range(0, 0, 1, Some([9, 8, 7]), None, 3, 8);
    assert!(
        grid.extras().is_empty(),
        "precondition: RGB-only data belongs in the ring, not the map — \
         otherwise this test does not exercise the map-empty gate"
    );

    let row = grid.storage.row(0).expect("row 0");
    let extracted = Grid::extract_row_extras(row, grid.extras(), 0);
    assert_eq!(
        extracted.rgb_fg,
        vec![(0u16, [9u8, 8, 7])],
        "the gate dropped a truecolor cell that lives in the ring"
    );
}

#[test]
fn armed_ring_does_not_change_a_plain_row() {
    let mut grid = Grid::new(3, 8);
    grid.set_cursor(0, 0);
    for ch in "hello".chars() {
        grid.write_char(ch);
    }

    let before = {
        let row = grid.storage.row(0).expect("row 0");
        Grid::extract_row_extras(row, grid.extras(), 0)
    };
    assert!(before.is_empty(), "a plain row extracts nothing");

    grid.extras_mut()
        .set_rgb_ring_range(2, 0, 4, Some([1, 2, 3]), None, 3, 8);
    assert!(
        grid.extras().has_any_data(),
        "precondition: the sticky grid-global gate is now armed"
    );
    assert!(
        grid.extras().is_empty(),
        "precondition: the map is still empty — only the ring was armed"
    );

    let after = {
        let row = grid.storage.row(0).expect("row 0");
        Grid::extract_row_extras(row, grid.extras(), 0)
    };
    assert!(
        after.is_empty(),
        "an armed ring on another row changed what a plain row extracts"
    );
}
