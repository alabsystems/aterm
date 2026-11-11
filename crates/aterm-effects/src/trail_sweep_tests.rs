// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn bounded_tail_matches_forward_bresenham_exhaustively() {
    fn reference(origin: (i32, i32), destination: (i32, i32)) -> Vec<(i32, i32)> {
        let ((r0, c0), (r1, c1)) = (origin, destination);
        let (dr, dc) = ((r1 - r0).abs(), (c1 - c0).abs());
        let (sr, sc) = (if r0 < r1 { 1 } else { -1 }, if c0 < c1 { 1 } else { -1 });
        let mut err = dc - dr;
        let (mut r, mut c) = (r0, c0);
        let mut cells = Vec::new();
        loop {
            cells.push((r, c));
            if (r, c) == (r1, c1) {
                return cells;
            }
            let e2 = 2 * err;
            if e2 > -dr {
                err -= dr;
                c += sc;
            }
            if e2 < dc {
                err += dc;
                r += sr;
            }
        }
    }

    let mut actual = Vec::new();
    for r0 in -4..=3 {
        for c0 in -4..=3 {
            for r1 in -4..=3 {
                for c1 in -4..=3 {
                    for include_destination in [false, true] {
                        for limit in 0..=10 {
                            let mut expected = reference((r0, c0), (r1, c1));
                            if !include_destination {
                                expected.pop();
                            }
                            let keep_from = expected.len().saturating_sub(limit);
                            expected.drain(..keep_from);
                            line_cells_tail(
                                &mut actual,
                                (r0, c0),
                                (r1, c1),
                                limit,
                                include_destination,
                            );
                            assert_eq!(
                                actual, expected,
                                "({r0},{c0})→({r1},{c1}), limit={limit}, include={include_destination}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn a_physical_margin_fold_runs_from_the_panes_last_column_to_its_first() {
    assert!(physical_margin_fold((5, 51), (6, 12), 12, 40));
    assert!(
        !physical_margin_fold((5, 51), (6, 11), 12, 40),
        "a neighboring pane cannot borrow the fold shape"
    );
}
