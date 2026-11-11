// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Tile;

#[test]
fn nan_alpha_leaves_painted_pixels_unchanged() {
    let mut t = Tile::new(4, 1);
    t.over_run(0, 0, 4, (1.0, 0.5, 0.25), 1.0);
    let painted = t.pixels().to_vec();
    assert_ne!(painted, vec![0; 16]);

    t.over(1, 0, (0.0, 1.0, 0.0), f32::NAN);
    t.over_run(0, 0, 4, (0.0, 0.0, 1.0), f32::NAN);
    assert_eq!(t.pixels(), &painted[..]);

    let mut huge = Tile::new(4, 1);
    huge.disc(2.0, 0.5, 2.0e20, (1.0, 1.0, 1.0), 1.0);
    assert_eq!(huge.pixels(), &[255; 16]);
}
