// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Additional acceptance-level tests for damage tracking.

use super::*;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
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

#[test]
fn merged_damage_iterator_full_damage_yields_single_rect() {
    let damage = Damage::Full;

    let rects: Vec<_> = damage.iter_merged(6, 20).collect();

    assert_eq!(rects, vec![DamageRect::new(0, 6, 0, 20)]);
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn mark_random_cells_tracks_expected_rows_and_bounds(
        cells in vec((0u16..64, 0u16..128), 0..256)
    ) {
        const ROWS: u16 = 64;
        const COLS: u16 = 128;

        let mut damage = Damage::new(ROWS);
        let mut expected = BTreeMap::<u16, (u16, u16)>::new();

        for (row, col) in cells {
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
        prop_assert_eq!(actual_rows, expected_rows);

        let expected_bounds: Vec<_> = expected
            .iter()
            .map(|(&line, &(left, right))| LineDamageBounds::new(line, left, right.min(COLS)))
            .filter(|bounds| !bounds.is_empty())
            .collect();
        let actual_bounds: Vec<_> = damage.iter_bounds(ROWS, COLS).collect();
        prop_assert_eq!(actual_bounds, expected_bounds);
    }
}
