// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{MAX_SELECTION_DAMAGE_BANDS, SelectionDamage};

const fn band(lo_abs: u64, hi_abs: u64) -> SelectionDamage {
    SelectionDamage::Band { lo_abs, hi_abs }
}

/// Collect the bands a value names, read through `clears_selection` (the
/// consumer-facing spelling), so a test can state the whole answer.
fn bands_of(damage: SelectionDamage) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let cleared = damage.clears_selection(|lo_abs, hi_abs| {
        out.push((lo_abs, hi_abs));
        false
    });
    assert!(!cleared, "a predicate that never overlaps cannot clear");
    out
}

/// The gap between two disjoint bands is not damage.
#[test]
fn disjoint_bands_stay_disjoint() {
    let damage = band(10, 10).union(band(30, 30));
    assert_eq!(bands_of(damage), vec![(10, 10), (30, 30)]);
    assert!(!damage.clears_selection(|lo_abs, hi_abs| lo_abs <= 20 && 20 <= hi_abs));
    assert!(damage.clears_selection(|lo_abs, hi_abs| lo_abs <= 30 && 30 <= hi_abs));
}

/// Adjacent and overlapping bands coalesce, which is what keeps a row-by-row
/// repaint from consuming a set slot per row.
#[test]
fn adjacent_and_overlapping_bands_coalesce() {
    let rows = band(3, 3).union(band(4, 4)).union(band(5, 5));
    assert_eq!(rows, band(3, 5));
    assert_eq!(band(3, 7).union(band(5, 9)), band(3, 9));
}

/// Order independence holds within the 8-band arity bound: a sequence whose
/// overlapping pieces coalesce within the bound keeps its gaps, folded in
/// either order.
#[test]
fn union_is_order_independent_within_the_arity_bound() {
    let seq = [
        (21u64, 24u64),
        (0, 2),
        (4, 7),
        (39, 42),
        (5, 5),
        (32, 36),
        (35, 39),
    ];
    let fold = |order: &mut dyn Iterator<Item = &(u64, u64)>| {
        order.fold(SelectionDamage::None, |acc, &(lo, hi)| {
            acc.union(band(lo, hi))
        })
    };
    let forward = fold(&mut seq.iter());
    let backward = fold(&mut seq.iter().rev());
    assert_eq!(
        forward, backward,
        "the same batch must damage the same rows whatever order its ops recorded in"
    );
    assert_eq!(bands_of(forward), vec![(0, 2), (4, 7), (21, 24), (32, 42)]);
}

/// Past the bound the set absorbs a pair, never collapsing to the hull, and covers every
/// damaged row.
#[test]
fn overflow_absorbs_a_pair_and_still_covers_every_damaged_row() {
    let ten = [
        (0u64, 1u64),
        (10, 11),
        (20, 21),
        (30, 31),
        (40, 41),
        (50, 51),
        (60, 61),
        (70, 71),
        (80, 81),
        (90, 91),
    ];
    let merged = ten.iter().fold(SelectionDamage::None, |acc, &(lo, hi)| {
        acc.union(band(lo, hi))
    });
    let got = bands_of(merged);
    assert!(
        got.len() <= MAX_SELECTION_DAMAGE_BANDS,
        "stays within the arity bound: {got:?}"
    );
    assert!(
        got.len() > 1,
        "degrades by absorbing, NOT to a single hull: {got:?}"
    );
    for pair in got.windows(2) {
        assert!(pair[0].1 < pair[1].0, "sorted and disjoint: {got:?}");
    }
    for (lo, hi) in ten {
        assert!(
            got.iter().any(|&(l, h)| l <= lo && hi <= h),
            "every recorded band stays covered — over-clear is safe, a stale \
             highlight is not: {lo}..={hi} missing from {got:?}"
        );
    }
}

/// At the bound the set is exact; one past it gives up a gap, not the whole set.
#[test]
fn past_the_arity_bound_one_gap_is_given_up_not_all_of_them() {
    let mut damage = SelectionDamage::None;
    for i in 0..MAX_SELECTION_DAMAGE_BANDS {
        let row = u64::try_from(i).unwrap_or(0) * 4;
        damage = damage.union(band(row, row));
    }
    assert_eq!(bands_of(damage).len(), MAX_SELECTION_DAMAGE_BANDS);
    assert!(
        !damage.clears_selection(|lo_abs, hi_abs| lo_abs <= 2 && 2 <= hi_abs),
        "at the bound the set is exact, so row 2 sits in a gap"
    );

    let overflowed = damage.union(band(100, 100));
    let got = bands_of(overflowed);
    assert!(
        got.len() <= MAX_SELECTION_DAMAGE_BANDS,
        "the bound is what makes this state fixed-size: {got:?}"
    );
    for i in 0..MAX_SELECTION_DAMAGE_BANDS {
        let row = u64::try_from(i).unwrap_or(0) * 4;
        assert!(
            got.iter().any(|&(l, h)| l <= row && row <= h),
            "every recorded row stays covered — over-clear is safe, a stale \
             highlight is not: {row} missing from {got:?}"
        );
    }
    assert!(
        got.iter().any(|&(l, h)| l <= 100 && 100 <= h),
        "including the one that caused the overflow: {got:?}"
    );
    assert!(
        !overflowed.clears_selection(|lo_abs, hi_abs| lo_abs <= 50 && 50 <= hi_abs),
        "row 50 is in the widest gap and nothing rewrote it; the hull degrade \
         this replaced cleared it, which is the regression: {got:?}"
    );
    assert_ne!(
        overflowed,
        band(0, 100),
        "…which is to say the set must not have collapsed to its hull"
    );
}

/// The two-band join answers what the sort-and-merge does for every pair of bands, the
/// degenerate ones (`lo > hi`) and the edges of `u64` included: generated pairs, and each of
/// them folded onto a running union, on both paths and by `union_band` in place.
#[test]
fn the_two_band_join_matches_the_merge() {
    let mut rng = aterm_spec::rng::XorShift::new(0x5E1E_C710_0B4D);
    let edges = [0u64, 1, 2, 3, 7, 8, u64::MAX - 2, u64::MAX - 1, u64::MAX];
    let pick = |rng: &mut aterm_spec::rng::XorShift| match rng.below(3) {
        0 => *rng.pick(&edges),
        1 => rng.below(12),
        _ => rng.next_u64(),
    };
    let (mut joined, mut apart) = (0usize, 0usize);
    let mut fast_acc = SelectionDamage::None;
    let mut slow_acc = SelectionDamage::None;
    let mut in_place = SelectionDamage::None;
    for _ in 0..200_000 {
        let a = band(pick(&mut rng), pick(&mut rng));
        let (b_lo, b_hi) = (pick(&mut rng), pick(&mut rng));
        let b = band(b_lo, b_hi);
        let fast = a.union(b);
        let slow = crate::test_counters::on_reference_paths(|| a.union(b));
        assert_eq!(fast, slow, "{a:?} ∪ {b:?}");
        match fast {
            SelectionDamage::Band { .. } => joined += 1,
            _ => apart += 1,
        }
        fast_acc = fast_acc.union(b);
        slow_acc = crate::test_counters::on_reference_paths(|| slow_acc.union(b));
        assert_eq!(fast_acc, slow_acc, "running union after {b:?}");
        in_place.union_band(b_lo, b_hi);
        assert_eq!(in_place, slow_acc, "in-place union after {b:?}");
        if rng.below(16) == 0 {
            (fast_acc, slow_acc, in_place) = Default::default();
        }
    }
    assert!(
        joined > 1000 && apart > 1000,
        "both arms: {joined} joined, {apart} apart"
    );
}

/// `record_row_band_moves(top, bottom, delta, times)` leaves the record `times` calls of
/// `record_row_band_move` leave: from records of every fill, near both `i16` ends, after a
/// move its band narrows, and with counts past the room left.
#[test]
fn repeated_band_moves_record_what_one_call_each_records() {
    use super::{RowBandMove, RowBandMoves};
    let mut rng = aterm_spec::rng::XorShift::new(0x5EED_BA4D);
    let mut composed_past_one = 0usize;
    for _ in 0..20_000 {
        let mut start = RowBandMoves::default();
        for _ in 0..rng.index(10) {
            let len = usize::from(start.len);
            if let Some(slot) = start.moves.get_mut(len) {
                let top = u16::try_from(rng.index(6)).unwrap_or(0);
                *slot = RowBandMove {
                    top,
                    bottom: top + u16::try_from(rng.index(6)).unwrap_or(0),
                    delta: match rng.index(4) {
                        0 => i16::MIN + i16::try_from(rng.index(4)).unwrap_or(0),
                        1 => i16::MAX - i16::try_from(rng.index(4)).unwrap_or(0),
                        _ => i16::try_from(rng.index(9)).unwrap_or(0) - 4,
                    },
                };
                start.len += 1;
            }
        }
        start.inexact = rng.index(5) == 0;
        let top = u16::try_from(rng.index(6)).unwrap_or(0);
        let bottom = top + u16::try_from(rng.index(6)).unwrap_or(0);
        let delta = i16::try_from(rng.index(7)).unwrap_or(0) - 3;
        let times = match rng.index(3) {
            0 => rng.index(4),
            1 => rng.index(40),
            _ => 32_000 + rng.index(4_000),
        };
        let mut once = crate::Grid::new(4, 4);
        let once = &mut once.storage.presentation;
        once.row_band_moves = start;
        once.record_row_band_moves(top, bottom, delta, times);
        let mut each = crate::Grid::new(4, 4);
        let each = &mut each.storage.presentation;
        each.row_band_moves = start;
        for _ in 0..times {
            each.record_row_band_move(top, bottom, delta);
        }
        assert_eq!(
            once.row_band_moves, each.row_band_moves,
            "from {start:?}: ({top}, {bottom}, {delta}) x {times}"
        );
        composed_past_one += usize::from(times > 1 && once.row_band_moves != start);
    }
    assert!(composed_past_one > 1000, "{composed_past_one} cases moved");
}

/// `None` is the unit and `All` absorbs, in both argument positions.
#[test]
fn none_is_the_unit_and_all_absorbs() {
    assert_eq!(SelectionDamage::None.union(band(1, 2)), band(1, 2));
    assert_eq!(band(1, 2).union(SelectionDamage::None), band(1, 2));
    assert_eq!(SelectionDamage::All.union(band(1, 2)), SelectionDamage::All);
    assert_eq!(
        band(1, 2).union(band(9, 9)).union(SelectionDamage::All),
        SelectionDamage::All
    );
    assert!(!SelectionDamage::None.clears_selection(|_, _| true));
    assert!(SelectionDamage::All.clears_selection(|_, _| false));
}
