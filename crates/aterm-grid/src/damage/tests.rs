// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn damage_full() {
    let mut damage = Damage::new(24);
    assert!(!damage.is_full());
    damage.mark_full();
    assert!(damage.is_full());
    assert!(damage.is_row_damaged(0));
    assert!(damage.is_row_damaged(23));
}

#[test]
fn damage_partial_row() {
    let mut damage = Damage::new(24);
    damage.mark_row(5);
    assert!(damage.is_row_damaged(5));
    assert!(!damage.is_row_damaged(4));
    assert!(!damage.is_row_damaged(6));
}

#[test]
fn damage_row_bounds_clamped_to_cols() {
    let mut damage = Damage::new(10);
    damage.mark_row(3);
    assert_eq!(damage.row_damage_bounds(3, 80), Some((0, 80)));
}

#[test]
fn damage_partial_cell() {
    let mut damage = Damage::new(24);
    damage.mark_cell(5, 10);
    damage.mark_cell(5, 20);
    assert!(damage.is_row_damaged(5));
    let bounds = damage.row_damage_bounds(5, 80);
    assert_eq!(bounds, Some((10, 21)));
}

#[test]
fn damage_reset() {
    let mut damage = Damage::new(24);
    damage.mark_full();
    assert!(damage.is_full());
    damage.reset(24);
    assert!(!damage.is_full());
    assert!(!damage.is_row_damaged(0));
}

#[test]
fn damage_iterator() {
    let mut damage = Damage::new(24);
    damage.mark_row(3);
    damage.mark_row(7);
    damage.mark_row(15);

    let damaged: Vec<_> = damage.damaged_rows(24).collect();
    assert_eq!(damaged, vec![3, 7, 15]);
}

#[test]
fn tracker_many_rows() {
    let mut tracker = DamageTracker::new(200);
    tracker.mark_row(150);
    assert!(tracker.is_row_damaged(150));
    assert!(!tracker.is_row_damaged(149));
    assert!(!tracker.is_row_damaged(151));
}

#[test]
fn damage_iterator_full() {
    let damage = Damage::Full;
    let damaged: Vec<_> = damage.damaged_rows(5).collect();
    assert_eq!(damaged, vec![0, 1, 2, 3, 4]);
}

#[test]
fn damage_iterator_sparse() {
    let mut damage = Damage::new(100);
    damage.mark_row(0);
    damage.mark_row(63); // End of first word
    damage.mark_row(64); // Start of second word
    damage.mark_row(99);

    let damaged: Vec<_> = damage.damaged_rows(100).collect();
    assert_eq!(damaged, vec![0, 63, 64, 99]);
}

#[test]
fn damage_iterator_empty() {
    let damage = Damage::new(24);
    let damaged: Vec<_> = damage.damaged_rows(24).collect();
    assert!(damaged.is_empty());
}

/// size_hint() does not panic after exhaustion.
#[test]
fn damage_iterator_size_hint_after_exhaustion() {
    let mut damage = Damage::new(24);
    damage.mark_row(5);

    let mut iter = damage.damaged_rows(24);

    let (_, upper) = iter.size_hint();
    assert!(
        upper.unwrap() <= 24,
        "upper bound should not exceed total rows"
    );

    assert_eq!(iter.next(), Some(5));
    assert_eq!(
        iter.next(),
        None,
        "iterator should be exhausted after single damaged row"
    );

    let (lower, upper) = iter.size_hint();
    assert_eq!(lower, 0);
    assert_eq!(upper, Some(0));
}

#[test]
fn damage_has_damage() {
    let mut damage = Damage::new(24);
    assert!(!damage.has_damage());

    damage.mark_cell(5, 10);
    assert!(damage.has_damage());

    damage.reset(24);
    assert!(!damage.has_damage());

    damage.mark_full();
    assert!(damage.has_damage());
}

#[test]
fn damage_iter_bounds() {
    let mut damage = Damage::new(24);
    damage.mark_cell(3, 10);
    damage.mark_cell(3, 20);
    damage.mark_row(7);
    damage.mark_cell(15, 5);

    let bounds: Vec<_> = damage.iter_bounds(24, 80).collect();
    assert_eq!(
        bounds,
        vec![
            LineDamageBounds::new(3, 10, 21),
            LineDamageBounds::new(7, 0, 80),
            LineDamageBounds::new(15, 5, 6),
        ]
    );
}

#[test]
fn bitset_iterator_single_word() {
    let bits = vec![0b10101010u64]; // Bits 1, 3, 5, 7 set
    let iter = BitsetRowIterator::new(&bits, 64);
    let rows: Vec<_> = iter.collect();
    assert_eq!(rows, vec![1, 3, 5, 7]);
}

#[test]
fn bitset_iterator_multi_word() {
    let mut bits = vec![0u64; 3];
    bits[0] = 1; // Row 0
    bits[1] = 1 << 5; // Row 69 (64 + 5)
    bits[2] = 1 << 10; // Row 138 (128 + 10)

    let iter = BitsetRowIterator::new(&bits, 200);
    let rows: Vec<_> = iter.collect();
    assert_eq!(rows, vec![0, 69, 138]);
}

#[test]
fn bitset_iterator_respects_max() {
    let bits = vec![u64::MAX]; // All 64 bits set
    let iter = BitsetRowIterator::new(&bits, 10); // Only want first 10
    let rows: Vec<_> = iter.collect();
    assert_eq!(rows, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

// State machine tests

#[test]
fn state_new_creates_partial() {
    let damage = Damage::new(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());
}

#[test]
fn state_mark_full_transitions() {
    let mut damage = Damage::new(24);
    assert!(!damage.is_full());
    damage.mark_full();
    assert!(damage.is_full());
    assert!(damage.has_damage());
}

#[test]
fn state_reset_transitions_full_to_partial() {
    let mut damage = Damage::new(24);
    damage.mark_full();
    assert!(damage.is_full());
    damage.reset(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());
}

#[test]
fn state_mark_row_preserves_partial() {
    let mut damage = Damage::new(24);
    damage.mark_row(5);
    assert!(!damage.is_full());
    assert!(damage.has_damage(), "marking should record damage");
    assert!(damage.is_row_damaged(5), "marked row should be damaged");
}

#[test]
fn state_mark_rows_preserves_partial() {
    let mut damage = Damage::new(24);
    damage.mark_rows(3, 10);
    assert!(!damage.is_full());
    assert!(damage.has_damage(), "marking should record damage");
    assert!(damage.is_row_damaged(5), "row in range should be damaged");
    assert!(
        !damage.is_row_damaged(2),
        "row before range should not be damaged"
    );
}

#[test]
fn state_mark_cell_preserves_partial() {
    let mut damage = Damage::new(24);
    damage.mark_cell(5, 10);
    assert!(!damage.is_full());
    assert!(damage.has_damage(), "marking should record damage");
    assert!(
        damage.is_row_damaged(5),
        "row with marked cell should be damaged"
    );
}

#[test]
fn state_full_operations_idempotent() {
    let mut damage = Damage::new(24);
    damage.mark_full();

    assert!(damage.is_row_damaged(5));
    damage.mark_row(5);
    assert!(damage.is_full());
    damage.mark_cell(5, 0);
    assert!(damage.is_full());
    damage.mark_full();
    assert!(damage.is_full());
}

#[test]
fn state_full_all_rows_damaged() {
    let damage = Damage::Full;
    for row in 0..100 {
        assert!(damage.is_row_damaged(row));
    }
}

#[test]
fn state_full_row_bounds_full_width() {
    let damage = Damage::Full;
    assert_eq!(damage.row_damage_bounds(5, 80), Some((0, 80)));
    assert_eq!(damage.row_damage_bounds(0, 200), Some((0, 200)));
}

#[test]
fn state_machine_cycle() {
    let mut damage = Damage::new(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());

    damage.mark_row(5);
    assert!(!damage.is_full());
    assert!(damage.has_damage());

    damage.mark_full();
    assert!(damage.is_full());
    assert!(damage.has_damage());

    damage.reset(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());
}

#[test]
fn state_partial_marked_is_damaged() {
    let mut damage = Damage::new(24);
    damage.mark_row(10);
    assert!(damage.is_row_damaged(10));
}

// DamageBoundsIterator correctness

/// DamageBoundsIterator skips rows whose column bounds fall entirely outside
/// the visible range (`mark_cell` with col >= cols) instead of ending early.
#[test]
fn damage_bounds_iterator_skips_out_of_range_row() {
    let mut damage = Damage::new(10);
    damage.mark_cell(3, 50); // Col 50 is beyond cols=20 → bounds clamp to None
    damage.mark_cell(5, 10); // Col 10 is within cols=20

    let bounds: Vec<_> = damage.iter_bounds(10, 20).collect();
    assert_eq!(bounds, vec![LineDamageBounds::new(5, 10, 11)]);
}

/// Multiple out-of-range rows interspersed with valid ones.
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

// Out-of-bounds access safety tests

#[test]
fn tracker_out_of_bounds_mark_row_is_safe() {
    let mut tracker = DamageTracker::new(10);
    tracker.mark_row(100); // Beyond capacity
    assert!(!tracker.is_row_damaged(100));
    assert_eq!(tracker.damaged_row_count(), 0);
}

#[test]
fn tracker_out_of_bounds_mark_cell_is_safe() {
    let mut tracker = DamageTracker::new(10);
    tracker.mark_cell(100, 5); // Row beyond capacity
    assert!(!tracker.is_row_damaged(100));
}

#[test]
fn tracker_out_of_bounds_is_row_damaged_returns_false() {
    let tracker = DamageTracker::new(10);
    assert!(!tracker.is_row_damaged(10)); // Exactly at capacity
    assert!(!tracker.is_row_damaged(100)); // Way beyond
}

#[test]
fn tracker_out_of_bounds_row_damage_bounds_returns_none() {
    let tracker = DamageTracker::new(10);
    assert_eq!(tracker.row_damage_bounds(10), None);
    assert_eq!(tracker.row_damage_bounds(100), None);
}

#[test]
fn mark_rows_range_behavior() {
    let mut damage = Damage::new(24);
    damage.mark_rows(5, 10);

    for row in 0..5 {
        assert!(
            !damage.is_row_damaged(row),
            "row {row} should not be damaged"
        );
    }
    for row in 5..10 {
        assert!(damage.is_row_damaged(row), "row {row} should be damaged");
    }
    for row in 10..24 {
        assert!(
            !damage.is_row_damaged(row),
            "row {row} should not be damaged"
        );
    }
}

#[test]
fn mark_rows_empty_range() {
    let mut damage = Damage::new(24);
    damage.mark_rows(5, 5); // empty range
    assert!(!damage.has_damage());
}

#[test]
fn row_damage_bounds_small_cols_clamps_to_none() {
    let mut damage = Damage::new(10);
    damage.mark_cell(3, 50); // cell at column 50
    assert_eq!(damage.row_damage_bounds(3, 10), None);
}

#[test]
fn row_damage_bounds_partial_clamp() {
    let mut damage = Damage::new(10);
    damage.mark_cell(3, 5);
    damage.mark_cell(3, 50); // beyond cols
    assert_eq!(damage.row_damage_bounds(3, 20), Some((5, 20)));
}

#[test]
fn row_damage_bounds_near_u16_max_boundary() {
    let mut damage = Damage::new(4);
    damage.mark_cell(2, u16::MAX - 1);
    assert_eq!(
        damage.row_damage_bounds(2, u16::MAX),
        Some((u16::MAX - 1, u16::MAX))
    );
}

#[test]
fn row_damage_bounds_merge_near_u16_max_boundary() {
    let mut damage = Damage::new(4);
    damage.mark_cell(1, u16::MAX - 2);
    damage.mark_cell(1, u16::MAX - 1);
    assert_eq!(
        damage.row_damage_bounds(1, u16::MAX),
        Some((u16::MAX - 2, u16::MAX))
    );
}

#[test]
fn tracker_mark_cell_u16_max_saturates_right_bound() {
    let mut tracker = DamageTracker::new(2);
    tracker.mark_cell(1, u16::MAX);
    assert_eq!(tracker.row_damage_bounds(1), Some((u16::MAX, u16::MAX)));
}

#[test]
fn damaged_row_count() {
    let mut tracker = DamageTracker::new(100);
    assert_eq!(tracker.damaged_row_count(), 0);

    tracker.mark_row(5);
    tracker.mark_row(50);
    tracker.mark_row(99);
    assert_eq!(tracker.damaged_row_count(), 3);

    // Marking same row again doesn't change count
    tracker.mark_row(5);
    assert_eq!(tracker.damaged_row_count(), 3);
}

// DamageTracker::clear() behavioral equivalence

/// clear() produces the same observable state as constructing a fresh tracker.
#[test]
fn tracker_clear_equivalent_to_new_same_size() {
    let mut tracker = DamageTracker::new(100);
    tracker.mark_row(5);
    tracker.mark_row(50);
    tracker.mark_cell(99, 42);

    tracker.clear(100);
    let fresh = DamageTracker::new(100);

    assert_eq!(tracker.row_bits, fresh.row_bits);
    assert_eq!(tracker.damaged_row_count(), 0);
    assert!(!tracker.is_row_damaged(5));
    assert!(!tracker.is_row_damaged(50));
    assert!(!tracker.is_row_damaged(99));
    assert_eq!(tracker.row_damage_bounds(99), None);
}

/// clear() with a different row count resizes correctly.
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

/// Damage::reset reuses allocations in the Partial→Partial path.
#[test]
fn damage_reset_reuses_allocation_partial_to_partial() {
    let mut damage = Damage::new(24);
    damage.mark_row(5);
    damage.mark_cell(10, 30);

    damage.reset(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());
    assert!(!damage.is_row_damaged(5));
    assert!(!damage.is_row_damaged(10));

    damage.mark_cell(3, 7);
    assert!(damage.has_damage());
    assert!(damage.is_row_damaged(3));
    assert_eq!(damage.row_damage_bounds(3, 80), Some((7, 8)));
}

/// Damage::reset from Full state creates fresh tracker.
#[test]
fn damage_reset_from_full_creates_fresh() {
    let mut damage = Damage::Full;
    assert!(damage.is_full());

    damage.reset(24);
    assert!(!damage.is_full());
    assert!(!damage.has_damage());

    damage.mark_row(0);
    assert!(damage.has_damage());
    assert!(damage.is_row_damaged(0));
}

/// Repeated reset cycles don't accumulate stale state.
#[test]
fn damage_reset_repeated_cycles() {
    let mut damage = Damage::new(24);
    for cycle in 0..100u16 {
        let row = cycle % 24;
        damage.mark_row(row);
        assert!(damage.is_row_damaged(row));
        damage.reset(24);
        assert!(
            !damage.has_damage(),
            "cycle {cycle}: damage should be clear after reset"
        );
    }
}

/// The row and bounds iterators are exact: `BitsetRowIterator` yields exactly
/// the set bits below `max_row`, in increasing order, against a bit-by-bit
/// reference over 8-bit patterns within a word and across a word boundary;
/// `DamageBoundsIterator` over one marked cell yields one bounds row covering
/// it when the cell is visible and nothing when it is not.
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
