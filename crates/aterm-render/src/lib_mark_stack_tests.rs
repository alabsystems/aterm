// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn mark_stack_dy_stacks_above_up_below_down_and_leaves_overlays_alone() {
    let (baseline, cell_h) = (100, 200);
    let mut s = MarkStack::default();
    let marks = [
        (10, 3),
        (10, 3),
        (-4, 2),
        (-3, 8),
        (-4, 2),
        (20, 2),
        (10, 3),
    ];
    let got: Vec<i32> = marks
        .iter()
        .map(|&(ink_ymin, ink_h)| s.dy(ink_h, ink_ymin, baseline, cell_h, 1))
        .collect();
    assert_eq!(
        got,
        [0, -4, 0, 0, 3, 0, -13],
        "first on each side 0; second acute lifted gh+1; overlay untouched; \
         second dot dropped gh+1; a mark already above the ceiling unmoved; \
         then the next acute clears that one"
    );
    for &(ink_ymin, ink_h) in &[(0, 1), (7, 12), (-1, 1), (-9, 9), (-2, 5)] {
        assert_eq!(
            MarkStack::default().dy(ink_h, ink_ymin, baseline, cell_h, 1),
            0,
            "({ink_ymin},{ink_h})"
        );
        assert_eq!(
            MarkStack::default().dy(ink_h, ink_ymin, baseline, cell_h, 2),
            0,
            "({ink_ymin},{ink_h}) 2x"
        );
    }
    let mut s2 = MarkStack::default();
    assert_eq!(s2.dy(3, 10, baseline, cell_h, 2), 0);
    assert_eq!(s2.dy(3, 10, baseline, cell_h, 2), -8);
    assert_eq!(s2.dy(2, -4, baseline, cell_h, 2), 0);
    assert_eq!(s2.dy(2, -4, baseline, cell_h, 2), 6);
}
