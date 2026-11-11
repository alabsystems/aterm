// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn material_master_preserves_real_characters() {
    let chars = ['C', 'c', '0', '{', '─'];
    let m = rasterize_material_master(&chars);
    for (g, ch) in chars.iter().enumerate() {
        let inked: u32 = (0..MASTER_H).map(|y| m.row(g, y).count_ones()).sum();
        assert!(inked > 0, "material character {ch:?} has ink");
    }
    for (a, b) in [(0usize, 1usize), (1, 2), (2, 3), (3, 4)] {
        assert!(
            (0..MASTER_H).any(|y| m.row(a, y) != m.row(b, y)),
            "distinct real characters {:?}/{:?} remain distinct",
            chars[a],
            chars[b]
        );
    }
    assert!(
        material_bitmap('🐈').is_none(),
        "unsupported art is skipped, never faked"
    );
    assert!(
        material_bitmap('\u{00a0}').is_none(),
        "an all-zero whitespace bitmap is not visible rain material"
    );
}
