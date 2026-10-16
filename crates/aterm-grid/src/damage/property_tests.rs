// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Additional acceptance-level tests for damage tracking.

use super::*;
use std::collections::BTreeMap;

#[test]
fn damage_bounds_iterator_empty_damage_returns_no_rows() {
    let damage = Damage::new(8);

    let bounds: Vec<_> = damage.iter_bounds(8, 20).collect();

    assert!(bounds.is_empty());
}

#[test]
fn damage_bounds_iterator_single_row_preserves_column_bounds() {
    let mut damage = Damage::new(8);
    damage.mark_cell(3, 4);
    damage.mark_cell(3, 9);

    let bounds: Vec<_> = damage.iter_bounds(8, 20).collect();

    assert_eq!(bounds, vec![LineDamageBounds::new(3, 4, 10)]);
}

/// Random cell marks: the damaged rows and per-row column bounds are exactly
/// the rows marked and the min/max column marked in each.
#[test]
fn mark_random_cells_tracks_expected_rows_and_bounds() {
    const ROWS: u16 = 64;
    const COLS: u16 = 128;
    let mut rng = aterm_spec::rng::XorShift::new(0xDA3A_6E00_0BAD_F00D);
    let mut next = move |n: u64| rng.below(n);
    for _ in 0..256 {
        let mut damage = Damage::new(ROWS);
        let mut expected = BTreeMap::<u16, (u16, u16)>::new();
        for _ in 0..next(256) {
            let (row, col) = (next(64) as u16, next(128) as u16);
            damage.mark_cell(row, col);
            expected
                .entry(row)
                .and_modify(|(left, right)| {
                    *left = (*left).min(col);
                    *right = (*right).max(col.saturating_add(1));
                })
                .or_insert((col, col.saturating_add(1)));
        }

        let expected_rows: Vec<_> = expected.keys().copied().collect();
        let actual_rows: Vec<_> = damage.damaged_rows(ROWS).collect();
        assert_eq!(actual_rows, expected_rows);

        let expected_bounds: Vec<_> = expected
            .iter()
            .map(|(&line, &(left, right))| LineDamageBounds::new(line, left, right.min(COLS)))
            .filter(|bounds| !bounds.is_empty())
            .collect();
        let actual_bounds: Vec<_> = damage.iter_bounds(ROWS, COLS).collect();
        assert_eq!(actual_bounds, expected_bounds);
    }
}
