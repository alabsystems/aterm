// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::BTreeMap;

#[test]
fn state_machine_cycle() {
    let mut damage = Damage::new(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());

    damage.mark_row(5);
    damage.mark_rows(7, 9);
    damage.mark_cell(12, 3);
    assert!(!damage.is_full());
    assert!(damage.has_damage());

    damage.mark_full();
    damage.mark_row(5);
    damage.mark_cell(5, 0);
    assert!(damage.is_full());
    assert!(damage.has_damage());

    damage.reset(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());
    damage.mark_row(0);
    assert_eq!(damage.damaged_rows(24).collect::<Vec<_>>(), vec![0]);
}

#[test]
fn full_damage_covers_every_row_and_column() {
    let damage = Damage::Full;
    assert_eq!(
        damage.damaged_rows(5).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert!((0..100).all(|row| damage.is_row_damaged(row)));
    assert_eq!(damage.row_damage_bounds(5, 80), Some((0, 80)));
    assert_eq!(damage.row_damage_bounds(0, 200), Some((0, 200)));
    assert_eq!(
        damage.iter_bounds(3, 80).collect::<Vec<_>>(),
        vec![
            LineDamageBounds::new(0, 0, 80),
            LineDamageBounds::new(1, 0, 80),
            LineDamageBounds::new(2, 0, 80),
        ]
    );
}

#[test]
fn damage_iterator_size_hint_after_exhaustion() {
    let mut damage = Damage::new(24);
    damage.mark_row(5);

    let mut iter = damage.damaged_rows(24);
    assert!(iter.size_hint().1.unwrap() <= 24);
    assert_eq!(iter.next(), Some(5));
    assert_eq!(iter.next(), None);
    assert_eq!(iter.size_hint(), (0, Some(0)));
}

#[test]
fn damage_bounds_iterator_skips_multiple_out_of_range() {
    let mut damage = Damage::new(10);
    damage.mark_cell(1, 100); // Out of range
    damage.mark_cell(2, 5); // Valid
    damage.mark_cell(3, 200); // Out of range
    damage.mark_cell(4, 10); // Valid
    damage.mark_cell(5, 150); // Out of range

    let bounds: Vec<_> = damage.iter_bounds(10, 20).collect();
    assert_eq!(
        bounds,
        vec![
            LineDamageBounds::new(2, 5, 6),
            LineDamageBounds::new(4, 10, 11),
        ]
    );
}

#[test]
fn row_damage_bounds_clamp_and_saturate() {
    let mut damage = Damage::new(10);
    damage.mark_row(3);
    assert_eq!(damage.row_damage_bounds(3, 80), Some((0, 80)), "whole row");
    damage.mark_cell(4, 5);
    damage.mark_cell(4, 50);
    assert_eq!(
        damage.row_damage_bounds(4, 20),
        Some((5, 20)),
        "partial clamp"
    );
    damage.mark_cell(2, u16::MAX - 2);
    damage.mark_cell(2, u16::MAX - 1);
    assert_eq!(
        damage.row_damage_bounds(2, u16::MAX),
        Some((u16::MAX - 2, u16::MAX)),
        "near u16::MAX"
    );

    let mut tracker = DamageTracker::new(2);
    tracker.mark_cell(1, u16::MAX);
    assert_eq!(tracker.row_damage_bounds(1), Some((u16::MAX, u16::MAX)));
}

#[test]
fn mark_rows_range_behavior() {
    let mut damage = Damage::new(24);
    damage.mark_rows(5, 5); // empty range
    assert!(!damage.has_damage());
    damage.mark_rows(5, 10);
    for row in 0..24 {
        assert_eq!(
            damage.is_row_damaged(row),
            (5..10).contains(&row),
            "row {row}"
        );
    }
}

#[test]
fn tracker_ignores_rows_past_its_capacity() {
    let mut tracker = DamageTracker::new(10);
    tracker.mark_row(100);
    tracker.mark_cell(100, 5);
    assert_eq!(tracker.damaged_row_count(), 0);
    for row in [10, 100] {
        assert!(!tracker.is_row_damaged(row));
        assert_eq!(tracker.row_damage_bounds(row), None);
    }
}

#[test]
fn tracker_clear_resizes_on_row_count_change() {
    let mut tracker = DamageTracker::new(24);
    tracker.mark_row(10);

    tracker.clear(100);
    let fresh = DamageTracker::new(100);

    assert_eq!(tracker.row_bits.len(), fresh.row_bits.len());
    assert_eq!(tracker.damaged_row_count(), 0);
    assert!(!tracker.is_row_damaged(10));

    tracker.mark_row(99);
    assert!(tracker.is_row_damaged(99));
}

#[test]
fn damage_reset_reuses_allocation_partial_to_partial() {
    let mut damage = Damage::new(24);
    damage.mark_row(5);
    damage.mark_cell(10, 30);
    damage.mark_cell(3, 70);

    damage.reset(24);
    assert!(!damage.has_damage());
    assert!(!damage.is_row_damaged(5));
    assert!(!damage.is_row_damaged(10));

    damage.mark_cell(3, 7);
    assert_eq!(damage.row_damage_bounds(3, 80), Some((7, 8)));
}

#[test]
fn bitset_and_bounds_iterators_are_exact() {
    let reference = |bits: &[u64], max_row: u16| -> Vec<u16> {
        (0..bits.len() * 64)
            .filter(|&r| bits[r / 64] >> (r % 64) & 1 == 1)
            .map(|r| u16::try_from(r).unwrap())
            .take_while(|&r| r < max_row)
            .collect()
    };
    let check = |bits: &[u64], max_row: u16| {
        let got: Vec<u16> = BitsetRowIterator::new(bits, max_row).collect();
        assert_eq!(got, reference(bits, max_row), "{bits:x?} below {max_row}");
        assert!(got.windows(2).all(|w| w[0] < w[1]));
    };
    for byte in 0..=0xFFu64 {
        for shift in [0, 28, 56] {
            for max_row in 0..=66 {
                check(&[byte << shift], max_row);
            }
        }
        for high in 0..=0xFFu64 {
            for max_row in [0, 1, 56, 63, 64, 65, 72, 127, 128] {
                check(&[high << 56, byte], max_row);
            }
        }
    }
    check(&[], 64);

    for rows in 1..=4u16 {
        for cols in 1..=16u16 {
            for row in 0..rows {
                for col in 0..=cols + 2 {
                    let mut damage = Damage::new(rows);
                    damage.mark_cell(row, col);
                    let bounds: Vec<_> = damage.iter_bounds(rows, cols).collect();
                    if col < cols {
                        let [b] = bounds.as_slice() else {
                            panic!("{rows}x{cols} ({row}, {col}): {bounds:?}");
                        };
                        assert_eq!(b.line, row);
                        assert!(b.left <= col && col < b.right && b.right <= cols, "{b:?}");
                    } else {
                        assert!(
                            bounds.is_empty(),
                            "{rows}x{cols} ({row}, {col}): {bounds:?}"
                        );
                    }
                }
            }
        }
    }
}

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
