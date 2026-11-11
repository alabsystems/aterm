// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::cell_ground;

/// A cell's ground is its commonest colour, and of two as common the first seen, as
/// `trail-contrast` takes one: neither the lower colour nor the one first seen later.
#[test]
fn a_cells_ground_is_its_commonest_colour_the_first_seen_of_a_tie() {
    let (dark, light) = (0x10_1010, 0xF0_F0F0);
    let mut counts = Vec::new();
    assert_eq!(
        cell_ground(&[light, dark, dark, light], &mut counts),
        Some(light)
    );
    assert_eq!(
        cell_ground(&[dark, light, light, dark], &mut counts),
        Some(dark)
    );
    assert_eq!(
        cell_ground(&[dark, light, light, light, dark], &mut counts),
        Some(light)
    );
    assert_eq!(cell_ground(&[], &mut counts), None);
}
