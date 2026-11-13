// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::cpu_damage_rects_into;

fn tuples(rects: &[crate::present::DamageRect]) -> Vec<(u32, u32, u32, u32)> {
    rects
        .iter()
        .map(|rect| (rect.x, rect.y, rect.width.get(), rect.height.get()))
        .collect()
}

#[test]
fn dirty_damage_geometry_is_exact() {
    let dirty = [true, true, false, true, false, true, true];
    let mut rects = Vec::new();
    assert!(cpu_damage_rects_into(
        &mut rects,
        &dirty,
        3,
        8,
        -5,
        55,
        |row| (row * 10, (row + 1) * 10)
    ));
    assert_eq!(
        tuples(&rects),
        vec![(3, 0, 8, 15), (3, 25, 8, 10), (3, 45, 8, 10)],
        "contiguous rows coalesce and top/bottom cropping stays exact",
    );

    for (dirty, span) in [([false, false], 8), ([true, true], 0)] {
        let mut rects = Vec::new();
        assert!(!cpu_damage_rects_into(
            &mut rects,
            &dirty,
            3,
            span,
            0,
            20,
            |row| (row * 10, (row + 1) * 10),
        ));
        assert_eq!(tuples(&rects), vec![(0, 0, 1, 1)]);
    }
}
