// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn streamed_slots_keep_windows_empty_bands_and_clone_position() {
    const BANDS: [ScaleBand; 4] = [
        ScaleBand {
            slots: 0,
            period_ms: 99,
            weight: 99,
            live_now: false,
        },
        ScaleBand {
            slots: 2,
            period_ms: 1_000,
            weight: 4,
            live_now: true,
        },
        ScaleBand {
            slots: 1,
            period_ms: 10_000,
            weight: 7,
            live_now: false,
        },
        ScaleBand {
            slots: 0,
            period_ms: 99,
            weight: 99,
            live_now: false,
        },
    ];
    let spec = MultiScaleSpec {
        bands: &BANDS,
        ticks: &[],
    };
    let expected = [
        ProgSlot {
            age_lo_ms: 0,
            age_hi_ms: 1_000,
            weight: 4,
            is_live_now: true,
            band: 1,
        },
        ProgSlot {
            age_lo_ms: 1_000,
            age_hi_ms: 2_000,
            weight: 4,
            is_live_now: true,
            band: 1,
        },
        ProgSlot {
            age_lo_ms: 2_000,
            age_hi_ms: 12_000,
            weight: 7,
            is_live_now: false,
            band: 2,
        },
    ];
    assert_eq!(spec.slots(), expected);
    let mut slots = spec.iter_slots();
    assert_eq!(slots.next(), Some(expected[0]));
    let mut copied = slots.clone();
    assert_eq!(slots.next(), Some(expected[1]));
    assert_eq!(slots.next(), Some(expected[2]));
    assert!(slots.next().is_none() && slots.next().is_none());
    assert_eq!(copied.next(), Some(expected[1]));
    assert_eq!(copied.next(), Some(expected[2]));
    assert!(copied.next().is_none() && copied.next().is_none());
    let mut empty = MultiScaleSpec {
        bands: &[],
        ticks: &[],
    }
    .iter_slots();
    assert!(empty.next().is_none() && empty.next().is_none());
}

#[test]
fn streamed_slots_keep_presets_and_saturated_band_indices() {
    for spec in [
        MultiScaleSpec::FULL_6H,
        MultiScaleSpec::FOLD_4H,
        MultiScaleSpec::COMPACT_6M,
        MultiScaleSpec::MINI_1M,
    ] {
        let slots: Vec<_> = spec.iter_slots().collect();
        assert_eq!(slots.len(), spec.slot_count());
        assert_eq!(slots.first().unwrap().age_lo_ms, 0);
        assert_eq!(slots.last().unwrap().age_hi_ms, spec.horizon_ms());
        for pair in slots.windows(2) {
            assert_eq!(pair[0].age_hi_ms, pair[1].age_lo_ms);
        }
        for slot in &slots {
            let band = spec.bands[usize::from(slot.band)];
            assert_eq!(slot.age_hi_ms - slot.age_lo_ms, band.period_ms);
            assert_eq!(slot.weight, band.weight);
            assert_eq!(slot.is_live_now, band.live_now);
        }
        assert_eq!(spec.slots(), slots);
    }
    const MANY: [ScaleBand; 257] = [ScaleBand {
        slots: 1,
        period_ms: 1,
        weight: 2,
        live_now: false,
    }; 257];
    for (i, slot) in (MultiScaleSpec {
        bands: &MANY,
        ticks: &[],
    })
    .iter_slots()
    .enumerate()
    {
        assert_eq!(slot.band, u8::try_from(i).unwrap_or(u8::MAX));
        assert_eq!(slot.age_lo_ms, i64::try_from(i).unwrap());
        assert_eq!(slot.age_hi_ms, i64::try_from(i + 1).unwrap());
    }
}

const T0: i64 = 1_792_000_800_000;

/// A reading at `at_ms` asked at the focused cadence.
fn tk(at_ms: i64) -> Taken {
    Taken::at(at_ms, 1_000)
}

#[test]
fn buckets_close_by_wall_clock() {
    let mut r = BucketRing::default();
    assert_eq!(r.push(tk(T0 + 9_900), Some(2.0), false), None);
    assert_eq!(
        r.push(tk(T0 + 10_000), Some(4.0), false),
        Some(T0 / BUCKET_MS),
        "a reading in the next bucket closes the open one"
    );
    assert_eq!(r.bucket(T0 / BUCKET_MS).and_then(Agg::value), Some(2.0));
    assert_eq!(r.bucket(T0 / BUCKET_MS + 1).and_then(Agg::value), Some(4.0));
    let _ = r.push(tk(T0 + 30_500), Some(6.0), false);
    let cells = r.cells();
    assert_eq!(cells.len(), 4);
    assert_eq!(cells[2], Cell::default(), "not read is None, never 0");
    assert_eq!(cells[3].mean, Some(6.0));
    let _ = r.push(tk(T0 + 40_000), None, false);
    assert_eq!(r.cells()[4].mean, None);
}

#[test]
fn the_tail_is_the_newest_buckets_with_the_open_one_last() {
    let mut r = BucketRing::default();
    assert!(r.tail_cells(3).is_empty());
    for i in [0, 1, 3, 4] {
        let _ = r.push(tk(T0 + i * 10_000), Some(i as f64), false);
    }
    let means = |cells: Vec<Cell>| cells.into_iter().map(|c| c.mean).collect::<Vec<_>>();
    assert_eq!(
        means(r.tail_cells(3)),
        [None, Some(3.0), Some(4.0)],
        "a gap bucket is unread, never 0"
    );
    assert_eq!(
        r.tail_cells(99),
        r.cells(),
        "asking past its length gives all"
    );
    assert!(r.tail_cells(0).is_empty());
    // The open bucket is last, its readings weighed by their time in it: 4 for the half
    // second of its slot inside the bucket, 8 for a whole second.
    let _ = r.push(tk(T0 + 45_000), Some(8.0), false);
    let last = means(r.tail_cells(1));
    assert_eq!(last.len(), 1);
    let want = (4.0 * 500.0 + 8.0 * 1_000.0) / 1_500.0;
    assert!((last[0].expect("read") - want).abs() < 1e-12, "{last:?}");
}

#[test]
fn columns_merge_on_wall_clock_multiples() {
    let mut r = BucketRing::default();
    for i in 0..10 {
        let v = (i != 4).then_some(f64::from(i));
        let _ = r.push(tk(T0 + i64::from(i) * 10_000), v, i == 7);
    }
    let now = T0 + 95_000;
    let ten = r.columns(10, 3, now);
    assert_eq!(
        ten.iter().map(|c| c.mean).collect::<Vec<_>>(),
        [Some(7.0), Some(8.0), Some(9.0)]
    );
    let twenty = r.columns(20, 3, now);
    assert_eq!(twenty[2].mean, Some(8.5));
    assert_eq!(twenty[1].mean, Some(6.5));
    assert_eq!(
        twenty[1].over_share(),
        0.5,
        "a column's over share counts its readings: one of two was over"
    );
    assert_eq!(twenty[1].max, Some(7.0));
    assert_eq!(twenty[0].mean, Some(5.0), "the unread bucket is skipped");
    let thirty = r.columns(30, 5, now);
    assert_eq!(thirty[4].mean, Some(9.0));
    assert_eq!(thirty[3].mean, Some(7.0));
    assert_eq!(thirty[2].mean, Some(4.0));
    assert_eq!(thirty[1].mean, Some(1.0));
    assert_eq!(thirty[0], Cell::default());
    let mut sparse = BucketRing::default();
    let _ = sparse.push(tk(T0), Some(1.0), false);
    let _ = sparse.push(tk(T0 + 40_000), Some(1.0), false);
    let cols = sparse.columns(20, 3, T0 + 40_000);
    assert_eq!(cols[1].mean, None);
    assert_eq!(cols[0].mean, Some(1.0));
    assert_eq!(columns_of(&[], i64::MAX, 10, 2, i64::MAX).len(), 2);
    assert_eq!(
        columns_of(&[Cell::default()], i64::MIN, 10, 4, i64::MIN).len(),
        4
    );
}

#[test]
fn unknown_is_never_zero_in_a_bucket_a_column_or_now() {
    let mut r = BucketRing::default();
    for i in 0..6 {
        let _ = r.push(tk(T0 + i * 10_000), None, false);
    }
    assert!(
        r.cells()
            .iter()
            .all(|c| c.mean.is_none() && c.max.is_none())
    );
    assert!(
        r.columns(30, 2, T0 + 55_000)
            .iter()
            .all(|c| c.mean.is_none())
    );
    assert_eq!(r.now_mean(T0 + 55_000, NOW_WINDOW_MS), None);
    assert_eq!(Agg::default().value(), None);
    assert_eq!(Agg::default().cell().over_share(), 0.0);
    let _ = r.push(tk(T0 + 60_000), Some(0.0), false);
    assert_eq!(r.now_mean(T0 + 60_000, NOW_WINDOW_MS), Some(0.0));
    assert_eq!(r.cells().last().and_then(|c| c.mean), Some(0.0));
}

#[test]
fn clock_steps_back() {
    let mut r = BucketRing::default();
    for i in 0..6 {
        let _ = r.push(tk(T0 + i * 10_000), Some(i as f64), false);
    }
    let open = r.open_k().expect("pushed");
    assert_eq!(r.push(tk(T0 + 25_000), Some(5.0), false), None);
    assert_eq!(r.open_k(), Some(open));
    assert_eq!(r.bucket(open).map(|b| b.n), Some(2));
    let before: Vec<Cell> = r.cells();
    let back = T0 + 25_000 - 2 * 3_600_000;
    let closed = r.push(tk(back), Some(9.0), false);
    let k = back.div_euclid(BUCKET_MS);
    assert_eq!(r.open_k(), Some(k));
    assert_eq!(closed, Some(k - 1));
    let after = r.cells();
    assert_eq!(&after[..before.len()], &before[..]);
    assert_eq!(after.last().and_then(|c| c.mean), Some(9.0));
    assert_eq!(r.now_mean(back, NOW_WINDOW_MS), Some(9.0));
    let _ = r.push(tk(back + 40_000), Some(1.0), false);
    let cells = r.cells();
    let n = cells.len();
    assert_eq!(cells[n - 4..n - 1], [Cell::default(); 3]);
    assert_eq!(cells[n - 1].mean, Some(1.0));
    let mut extremes = BucketRing::default();
    let _ = extremes.push(tk(i64::MAX), Some(1.0), false);
    let _ = extremes.push(tk(i64::MIN), Some(2.0), false);
    assert_eq!(extremes.open_k(), Some(i64::MIN.div_euclid(BUCKET_MS)));
    assert_eq!(extremes.now_mean(i64::MIN, NOW_WINDOW_MS), Some(2.0));
}

#[test]
fn the_ring_keeps_an_hour_and_a_long_silence_is_unread() {
    let mut r = BucketRing::default();
    for i in 0..400 {
        let _ = r.push(tk(T0 + i * 10_000), Some(1.0), false);
    }
    assert_eq!(r.len(), RING_BUCKETS);
    assert_eq!(r.open_k(), Some(T0 / BUCKET_MS + 399));
    let _ = r.push(tk(T0 + 399 * 10_000 + 2 * 3_600_000), Some(1.0), false);
    assert_eq!(r.len(), 1, "two silent hours leave nothing to show");
}

#[test]
fn now_is_the_trailing_mean_whatever_the_bucket_boundary() {
    let mut edge = Recent::default();
    edge.push(i64::MIN, 4.0);
    assert_eq!(edge.mean(i64::MIN, NOW_WINDOW_MS), Some(4.0));
    assert_eq!(edge.mean(i64::MIN, 0), None);
    assert_eq!(edge.mean(i64::MIN, -1), None);
    edge.push(i64::MIN + RECENT_KEEP_MS, 8.0);
    assert_eq!(edge.mean(i64::MIN + RECENT_KEEP_MS, i64::MAX), Some(8.0));
    edge.push(i64::MAX, 16.0);
    assert_eq!(edge.mean(i64::MAX, NOW_WINDOW_MS), Some(16.0));
    assert_eq!(edge.mean(i64::MAX, i64::MIN), None);
    // Readings on the half-slot, each standing for its own second.
    let mut r = BucketRing::default();
    for i in 0..15 {
        let _ = r.push(tk(T0 + i * 1000 + 500), Some(i as f64), false);
    }
    assert_eq!(r.now_mean(T0 + 14_500, NOW_WINDOW_MS), Some(9.5));
    assert_eq!(
        r.bucket(T0 / BUCKET_MS + 1).and_then(Agg::value),
        Some(12.0)
    );
}

#[test]
fn the_sustained_value_needs_three_read_buckets_and_ignores_one_spike() {
    let mut r = BucketRing::default();
    for (i, v) in [5.0, 6.0, 90.0, 7.0, 6.0].into_iter().enumerate() {
        let _ = r.push(tk(T0 + i as i64 * 10_000), Some(v), false);
    }
    let k0 = T0 / BUCKET_MS;
    assert_eq!(r.sustained(k0 + 1), None, "only two buckets before it");
    assert_eq!(r.sustained(k0 + 2), Some(5.0));
    assert_eq!(r.sustained(k0 + 3), Some(6.0), "the spike is not sustained");
    assert_eq!(r.sustained(k0 + 4), Some(6.0));
}

#[test]
fn a_ring_keeps_its_readings_with_their_over_marks() {
    let mut r = BucketRing::default();
    let _ = r.push(tk(T0), Some(1.0), false);
    let _ = r.push(tk(T0 + 1_000), None, false);
    let _ = r.push(tk(T0 + 2_000), Some(3.0), true);
    assert_eq!(
        r.readings(),
        [
            Reading {
                at_ms: T0,
                v: 1.0,
                over: false,
                w_ms: 1_000,
            },
            Reading {
                at_ms: T0 + 2_000,
                v: 3.0,
                over: true,
                w_ms: 1_000,
            },
        ],
        "an unread reading is no reading"
    );
    // The minute it keeps, and no more.
    let _ = r.push(tk(T0 + 2_000 + RECENT_KEEP_MS), Some(4.0), false);
    assert_eq!(r.readings().len(), 1);
}

#[test]
fn multi_scale_presets_read_outward_from_now_with_older_boxes_wider() {
    for spec in [
        MultiScaleSpec::FULL_6H,
        MultiScaleSpec::FOLD_4H,
        MultiScaleSpec::COMPACT_6M,
        MultiScaleSpec::MINI_1M,
    ] {
        let slots = spec.slots();
        assert_eq!(slots.len(), spec.slot_count());
        // Newest first: the live box at the left end, holding now.
        let live = slots[0];
        assert!(live.is_live_now && live.age_lo_ms == 0);
        assert!(slots[1..].iter().all(|s| !s.is_live_now));
        assert_eq!(slots.last().unwrap().age_hi_ms, spec.horizon_ms());
        for pair in slots.windows(2) {
            assert_eq!(
                pair[0].age_hi_ms, pair[1].age_lo_ms,
                "the boxes tile the age"
            );
        }
        for pair in slots.windows(2) {
            assert!(
                pair[0].band == pair[1].band || pair[0].band + 1 == pair[1].band,
                "each slot names its band, in order: {pair:?}"
            );
            assert_eq!(
                pair[0].band == pair[1].band,
                pair[0].age_hi_ms - pair[0].age_lo_ms == pair[1].age_hi_ms - pair[1].age_lo_ms
                    && pair[0].is_live_now == pair[1].is_live_now,
                "a band is its boxes' period: {pair:?}"
            );
        }
        assert!(
            spec.ticks.windows(2).all(|w| w[0].0 < w[1].0),
            "ticks read outward from now: tick_ages takes them coarsest first by reversing \
             this order"
        );
    }
    assert_eq!(MultiScaleSpec::FULL_6H.horizon_ms(), 6 * 3_600_000);
    assert_eq!(MultiScaleSpec::FOLD_4H.horizon_ms(), 4 * 3_600_000);
    assert_eq!(MultiScaleSpec::COMPACT_6M.horizon_ms(), 360_000);
    assert_eq!(MultiScaleSpec::MINI_1M.horizon_ms(), 60_000);
}

/// The owner's blocks (2026-10-26): 1 s, 10 s, 1 min, 10 min and 1 h, at least three of each
/// size a strip draws and five of each in Full, each size starting at the age of its own
/// block, where a tick names it; a narrower preset drops the largest sizes, never a middle
/// one.
#[test]
fn every_block_size_starts_at_its_own_age_with_three_to_five_of_it() {
    const SIZES: [i64; 5] = [1_000, 10_000, 60_000, 600_000, 3_600_000];
    for (spec, sizes, least) in [
        (MultiScaleSpec::FULL_6H, 5, 5),
        (MultiScaleSpec::FOLD_4H, 5, 3),
        (MultiScaleSpec::COMPACT_6M, 3, 5),
        (MultiScaleSpec::MINI_1M, 2, 5),
    ] {
        let slots = spec.slots();
        let mut age = 0;
        for &size in &SIZES[..sizes] {
            let of: Vec<&ProgSlot> = slots
                .iter()
                .filter(|s| s.age_hi_ms - s.age_lo_ms == size)
                .collect();
            assert!(of.len() >= least, "{size} ms: {} boxes", of.len());
            assert_eq!(of[0].age_lo_ms, age, "{size} ms blocks follow the smaller");
            assert!(
                of.windows(2).all(|w| w[0].age_hi_ms == w[1].age_lo_ms),
                "{size} ms blocks stand together"
            );
            if size > 1_000 {
                assert_eq!(age, size, "a size starts where its age is its own block");
                assert!(
                    spec.ticks.iter().any(|&(at, _)| at == size),
                    "a tick names where {size} ms blocks start"
                );
            }
            age = of.last().map_or(age, |s| s.age_hi_ms);
        }
        assert_eq!(age, spec.horizon_ms(), "no other size is drawn");
        assert_eq!(spec.ticks.last().map(|t| t.0), Some(spec.horizon_ms()));
    }
}

/// Seven hours of ten-minute buckets (`LONG_BUCKETS` 43, record §5.2): the 1 h box leaving
/// the 6 h horizon still has its children. Each folds the closed ten-second buckets it holds,
/// as they close.
#[test]
fn a_ring_keeps_seven_hours_of_ten_minute_buckets_and_restores_them() {
    let mut r = BucketRing::default();
    assert_eq!((r.long_k(), r.long_cells().len()), (None, 0));
    let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
    for i in 0..8 * 360 {
        let at = t0 + i * 10_000;
        let _ = r.push(
            tk(at),
            Some(if at < t0 + 3_000_000 { 1.0 } else { 3.0 }),
            false,
        );
    }
    let k_now = (t0 + (8 * 360 - 1) * 10_000).div_euclid(LONG_BUCKET_MS);
    assert_eq!(r.long_k(), Some(k_now));
    let cells = r.long_cells();
    assert_eq!(cells.len(), LONG_BUCKETS, "seven hours and the open bucket");
    assert!(
        cells.iter().all(|c| c.mean == Some(3.0)),
        "the first fifty minutes aged out"
    );
    assert_eq!(r.len(), RING_BUCKETS, "the 10 s ring still keeps its hour");

    let saved = r.saved();
    assert_eq!(saved.long.len(), LONG_BUCKETS);
    let now = t0 + 8 * 3_600_000 - 1;
    let back = BucketRing::restored(&saved, now);
    assert_eq!(back.long_cells(), r.long_cells());
    assert_eq!(back.long_k(), r.long_k());
    let later = BucketRing::restored(&saved, now + 3 * 3_600_000);
    assert_eq!(
        later.long_cells().len(),
        LONG_BUCKETS - 18,
        "three hours on, the oldest three are gone"
    );
    let old = SavedRing {
        long: Vec::new(),
        ..saved
    };
    assert!(
        BucketRing::restored(&old, now).long_cells().is_empty(),
        "a file written before the long buckets restores none"
    );
    // The open ten-second bucket is not folded yet: the newest ten-minute bucket holds the
    // 59 closed before it, each one reading at its start (half a second of it).
    let newest = r.long_cells().last().copied().expect("read");
    assert_eq!(newest.tau_ms, 59.0 * 500.0);
}

/// Record §5.2 (`folded_below`): a ring restored from a base whose newest bucket a tail line
/// already folded folds it no second time when the next reading closes it, so its ten-minute
/// bucket holds the 20 s actually read. CONTROL: the same restore with its fold mark lowered
/// to that bucket, as the mark would stand were it not kept, folds it again, 30 s for 20.
#[test]
fn a_restored_ring_folds_its_newest_bucket_once() {
    let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
    let mut r = BucketRing::default();
    for i in 0..20 {
        let _ = r.push(tk(t0 + 500 + i * 1_000), Some(1.0), false);
    }
    let k1 = t0 / BUCKET_MS + 1;
    assert_eq!(r.open_k(), Some(k1));
    let mut saved = r.saved();
    saved.put_closed(k1, *r.bucket(k1).expect("the second bucket"));
    assert_eq!(saved.folded_below, k1 + 1);
    let now = t0 + 20_500;
    let long_tau = |saved: &SavedRing| {
        let mut back = BucketRing::restored(saved, now);
        let _ = back.push(tk(now), Some(1.0), false);
        back.long_cells().last().map(|c| c.tau_ms)
    };
    assert_eq!(
        long_tau(&saved),
        Some(20_000.0),
        "20 s read, each folded once"
    );
    let unmarked = SavedRing {
        folded_below: k1,
        ..saved
    };
    assert_eq!(
        long_tau(&unmarked),
        Some(30_000.0),
        "CONTROL: an unkept mark folds the restored bucket twice"
    );
}

/// F4′: the 10 s now-mean, the live box's column and the row's %, weighs each reading by the
/// time it stands for: a 5 s background reading of 0 beside five focused readings of 10 is
/// half each. Negative control: `Recent::mean` weighing each reading alike (the mutant the
/// F4′ review ran) gives the count-weighted 50/6, which the scenario keeps apart from 5.
#[test]
fn the_now_mean_weighs_each_reading_by_its_time() {
    let mut r = BucketRing::default();
    let _ = r.push(Taken::at(T0 + 2_500, 5_000), Some(0.0), false);
    for i in 0..5 {
        let _ = r.push(Taken::at(T0 + 5_500 + i * 1_000, 1_000), Some(10.0), false);
    }
    let now = T0 + 9_500;
    let mean = r.now_mean(now, NOW_WINDOW_MS).expect("read");
    assert!((mean - 5.0).abs() < 1e-12, "five seconds of each: {mean}");
    let readings = r.readings();
    let count = readings.iter().map(|x| x.v).sum::<f64>() / readings.len() as f64;
    assert!(
        (count - 50.0 / 6.0).abs() < 1e-12 && (count - mean).abs() > 3.0,
        "the scenario: its count-weighted mean {count} stands apart"
    );
}

/// A1 §1.4 item 9 (`lands`): a ten-minute history re-keyed by a fold more than
/// [`CLAMP_BACK_S`] behind moves its newest reading back with its buckets, so a fold 10 s
/// behind the re-keyed newest, across a ten-minute edge, lands in the newest bucket as the
/// clamp says. CONTROL: a fold 70 s behind, past the clamp, re-keys again.
#[test]
fn a_rekeyed_long_clamps_the_next_fold_by_its_moved_newest() {
    let e = T0 - T0.rem_euclid(LONG_BUCKET_MS) + 10 * LONG_BUCKET_MS;
    let ek = e / LONG_BUCKET_MS;
    let agg = |v: f64| {
        let mut a = Agg::default();
        a.add(v, 10_000, false);
        a
    };
    let mut long = Long::default();
    long.fold(e + 290_000, &agg(1.0));
    long.fold(e - LONG_BUCKET_MS, &agg(2.0));
    assert_eq!(
        (long.first_k, long.open_k()),
        (ek - 2, Some(ek - 1)),
        "fifteen minutes back re-keys"
    );
    long.fold(e - LONG_BUCKET_MS - BUCKET_MS, &agg(3.0));
    assert_eq!(
        (long.first_k, long.open_k()),
        (ek - 2, Some(ek - 1)),
        "10 s behind the moved newest lands in it"
    );
    assert_eq!(long.b.back().map(|a| (a.n, a.max)), Some((2, 3.0)));
    let mut past = long.clone();
    past.fold(e - LONG_BUCKET_MS - 70_000, &agg(4.0));
    assert_eq!(
        (past.first_k, past.open_k()),
        (ek - 4, Some(ek - 2)),
        "CONTROL: 70 s behind re-keys"
    );
}

#[test]
fn hour_boxes_read_the_ten_minute_buckets_by_how_much_of_each_they_span() {
    let mut r = BucketRing::default();
    let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
    for i in 0..6 * 360 {
        let at = t0 + i * 10_000;
        let v = (at.div_euclid(LONG_BUCKET_MS) - t0 / LONG_BUCKET_MS) as f64;
        let _ = r.push(tk(at), Some(v), false);
    }
    let now = t0 + 6 * 3_600_000 - 1 + 300_000;
    let (ring, long, recent) = (r.cells(), r.long_cells(), r.readings());
    let tiers = Tiers {
        ring: &ring,
        ring_k: r.open_k().unwrap_or(0),
        long: &long,
        long_k: r.long_k().unwrap_or(0),
        recent: &recent,
    };
    let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
    let hours: Vec<&ScaleCell> = cells
        .iter()
        .filter(|c| c.slot.age_hi_ms - c.slot.age_lo_ms == 3_600_000)
        .collect();
    assert_eq!(hours.len(), 5);
    // Bucket k of the six hours read k - first, so an hour box reads the mean of its
    // buckets' keys weighted by how many of its ms each holds.
    let first = r.long_k().unwrap_or(0) + 1 - long.len() as i64;
    assert_eq!(first, t0 / LONG_BUCKET_MS);
    for h in &hours {
        let (lo, hi) = (now - h.slot.age_hi_ms, now - h.slot.age_lo_ms);
        let (mut sum, mut ms) = (0.0, 0.0);
        for k in lo.div_euclid(LONG_BUCKET_MS)..=(hi - 1).div_euclid(LONG_BUCKET_MS) {
            let ov = hi.min((k + 1) * LONG_BUCKET_MS) - lo.max(k * LONG_BUCKET_MS);
            sum += (k - first) as f64 * ov as f64;
            ms += ov as f64;
        }
        let got = h.cell.mean.expect("an hour the ring read");
        assert!(
            (got - sum / ms).abs() < 1e-6,
            "{got} against {}: {h:?}",
            sum / ms
        );
    }
    assert!(
        hours.windows(2).all(|w| w[0].cell.mean > w[1].cell.mean),
        "older hours read the older, lower buckets"
    );
    assert!(
        cells
            .iter()
            .filter(|c| c.slot.age_lo_ms < RING_REACH_MS && c.slot.age_lo_ms >= 600_000)
            .all(|c| c.cell.mean.is_some()),
        "the 10 min boxes read the ring"
    );
}

#[test]
fn multi_scale_marks_sleep_and_crash_gaps_without_smearing_readings() {
    let mut ring = BucketRing::default();
    let now = T0 + 120_000;
    let _ = ring.push(tk(now - 90_000), Some(20.0), false);
    let _ = ring.push(tk(now - 10_000), Some(50.0), false);
    let _ = ring.push(tk(now - 1_000), Some(80.0), false);
    let gaps = [
        TimeGap {
            start_ms: now - 50_000,
            end_ms: now - 20_000,
            kind: GapKind::Sleep,
        },
        TimeGap {
            start_ms: now - 8_000,
            end_ms: now - 4_000,
            kind: GapKind::Crash,
        },
    ];
    let (cells, recent) = (ring.cells(), ring.readings());
    let tiers = Tiers {
        ring: &cells,
        ring_k: ring.open_k().unwrap_or(0),
        recent: &recent,
        ..Tiers::default()
    };
    let scaled = multi_scale_cells(tiers, now, MultiScaleSpec::COMPACT_6M, 15_000, &gaps);
    assert!(scaled[0].slot.is_live_now, "newest first");
    assert_eq!(scaled[0].cell.mean, Some(80.0));
    assert!(
        scaled
            .iter()
            .any(|s| s.gap == Some(GapKind::Sleep) && s.cell.mean.is_none())
    );
    assert!(
        scaled
            .iter()
            .any(|s| s.gap == Some(GapKind::Crash) && s.cell.mean.is_none())
    );
}

/// A 10 s box older than a run's first reading draws nothing from that reading's bucket,
/// whose readings were all taken after the box ended; the buckets before it, an earlier
/// run's restored ones, still speak for it.
#[test]
fn a_10s_box_before_the_first_reading_draws_nothing_read_after_it() {
    let now = T0 + 15_000;
    let box_at = |ring: &BucketRing, age_lo_ms: i64, age_hi_ms: i64| -> Option<f64> {
        let (cells, long, recent) = (ring.cells(), ring.long_cells(), ring.readings());
        let tiers = Tiers {
            ring: &cells,
            ring_k: ring.open_k().unwrap_or(0),
            long: &long,
            long_k: ring.long_k().unwrap_or(0),
            recent: &recent,
        };
        let scaled = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 15_000, &[]);
        let s = scaled
            .iter()
            .find(|s| s.slot.age_lo_ms == age_lo_ms && s.slot.age_hi_ms == age_hi_ms)
            .expect("the spec has the box");
        s.cell.mean
    };
    // A run whose first reading is mid-bucket, five seconds into T0's 10 s, then one a second.
    let read_on = |ring: &mut BucketRing| {
        for t in (5_000..=15_000).step_by(1_000) {
            let _ = ring.push(tk(T0 + t), Some(1.0), false);
        }
    };
    let mut fresh = BucketRing::default();
    read_on(&mut fresh);
    assert_eq!(
        box_at(&fresh, 9_000, 10_000),
        Some(1.0),
        "the first reading"
    );
    assert_eq!(
        box_at(&fresh, 10_000, 20_000),
        None,
        "the 10 s before the first reading: nothing was read"
    );

    // An earlier run read until 20 s before T0 and is restored with its buckets alone (a
    // strip with no per-second readings, `SavedRing::recent` empty), then the same run reads.
    let mut earlier = BucketRing::default();
    for t in (-100_000_i64..=-20_000).step_by(1_000) {
        let _ = earlier.push(tk(T0 + t), Some(2.0), false);
    }
    let mut saved = earlier.saved();
    saved.recent.clear();
    let mut restored = BucketRing::restored(&saved, T0 + 4_000);
    read_on(&mut restored);
    assert_eq!(
        box_at(&restored, 10_000, 20_000),
        None,
        "between the earlier run's last bucket and the first reading: nothing was read"
    );
    for (lo, hi) in [
        (30_000, 40_000),
        (40_000, 50_000),
        (50_000, 60_000),
        (60_000, 120_000),
    ] {
        assert_eq!(
            box_at(&restored, lo, hi),
            Some(2.0),
            "the earlier run's buckets at {lo}..{hi} ms"
        );
    }
    assert_eq!(
        multi_scale_cells(
            Tiers {
                ring: &[Cell::default()],
                ring_k: i64::MAX,
                long: &[Cell::default()],
                long_k: i64::MAX.div_euclid(LONG_BUCKET_MS),
                recent: &[],
            },
            i64::MAX,
            MultiScaleSpec::FULL_6H,
            2_500,
            &[],
        )
        .len(),
        MultiScaleSpec::FULL_6H.slot_count()
    );
}

/// A1 §1.4 item 9: `Agg` keeps `Bucket`'s guards. A value that is not finite is dropped; the
/// count saturates at `u16::MAX` and the read time at `u32::MAX`, past which a reading moves
/// only the count and the extremes; a merge brings nothing from a bucket no reading could
/// have left. `Cell::merge` and `Cell::merge_parts` ignore non-finite cells. CONTROL: the full
/// bucket with the count a wrapping add leaves (0) is one `is_sound` refuses, so a save and a
/// merge would drop it.
#[test]
fn agg_drops_non_finite_and_saturates_its_count() {
    let mut a = Agg::default();
    a.add(f64::NAN, 1_000, true);
    a.add(f64::INFINITY, 1_000, true);
    a.add(f64::NEG_INFINITY, 1_000, false);
    assert_eq!(a, Agg::default(), "non-finite readings are dropped");
    a.add(0.5, 0, false);
    assert_eq!((a.n, a.tau_ms), (1, 1), "a reading weighs at least 1 ms");

    let mut full = Agg {
        tau_ms: u32::MAX - 500,
        n: u16::MAX - 1,
        over_ms: 0,
        mean: 0.5,
        min: 0.5,
        max: 0.5,
    };
    full.add(0.9, 1_000, true);
    assert_eq!(full.n, u16::MAX);
    assert_eq!(full.tau_ms, u32::MAX);
    assert_eq!(full.over_ms, 500, "only the time it could still hold");
    full.add(0.1, 1_000, true);
    assert_eq!(
        (full.n, full.tau_ms, full.over_ms),
        (u16::MAX, u32::MAX, 500)
    );
    assert_eq!((full.min, full.max), (0.1, 0.9), "the extremes still carry");
    let mean = full.mean;
    full.add(1.0, 1_000, false);
    assert_eq!(full.mean, mean, "a saturated read time moves no mean");
    assert!(full.is_sound());
    let wrapped = Agg {
        n: (u16::MAX - 1).wrapping_add(2),
        ..full
    };
    let mut into = Agg::default();
    into.merge(&wrapped);
    assert!(
        !wrapped.is_sound() && into == Agg::default(),
        "CONTROL: a count that wrapped to 0 makes the full bucket one a save and a merge drop"
    );

    let mut m = Agg::default();
    m.add(0.25, 4_000, true);
    let before = m;
    m.merge(&Agg {
        tau_ms: 3_000,
        n: 3,
        over_ms: 3_000,
        mean: f64::NAN,
        min: f64::NAN,
        max: f64::INFINITY,
    });
    assert_eq!(
        m, before,
        "a merge drops a bucket no reading could have left"
    );

    let nan_cell = Cell {
        mean: Some(f64::NAN),
        max: Some(f64::INFINITY),
        tau_ms: 3_000.0,
        over_ms: 3_000.0,
    };
    let finite_cell = Cell {
        mean: Some(0.25),
        max: Some(0.75),
        tau_ms: 4_000.0,
        over_ms: 4_000.0,
    };
    assert_eq!(Cell::merge([nan_cell, finite_cell]), finite_cell);
    assert_eq!(
        Cell::merge_parts([(nan_cell, 0.5), (finite_cell, 1.0)]),
        finite_cell
    );
}

/// A box's mean and over share weigh each bucket by its read time (A1 §3, time-weighted since
/// F4′): a bucket read for 10 s at 0.2 and one read for 2 s at 0.8 merge to 0.3, not the
/// buckets' 0.5; and an hour box that spans half of a ten-minute bucket takes half of that
/// bucket's read time and half of its over time. FULL_6H's newest hour box, at a now halfway
/// through a ten-minute bucket, spans half of its oldest bucket (read 10 min at 0.9, all
/// over), five whole ones (read 5 min each at 0.3) and half of its newest (5 min at 0.3):
/// 32.5 min read, 5 over, mean 153 / 390. CONTROL: weighing each bucket by its part alone
/// reads 0.35, and counting the half bucket's over time whole reads 120 / 390.
#[test]
fn a_merge_weighs_each_reading_by_its_time_and_a_part_brings_its_share_of_the_over() {
    let cell = |mean: f64, tau_ms: f64, over_ms: f64| Cell {
        mean: Some(mean),
        max: Some(mean),
        tau_ms,
        over_ms,
    };
    let two = Cell::merge([cell(0.2, 10_000.0, 0.0), cell(0.8, 2_000.0, 0.0)]);
    assert!((two.mean.unwrap() - 0.3).abs() < 1e-12, "{two:?}");
    assert_eq!((two.tau_ms, two.max), (12_000.0, Some(0.8)));
    assert!(
        (two.mean.unwrap() - 0.5).abs() > 0.1,
        "CONTROL: not the buckets' mean"
    );

    let k0 = T0.div_euclid(LONG_BUCKET_MS);
    assert_eq!(
        T0.rem_euclid(LONG_BUCKET_MS),
        0,
        "CONTROL: T0 opens a bucket"
    );
    let now = T0 + 7 * LONG_BUCKET_MS + LONG_BUCKET_MS / 2;
    let long_k = now.div_euclid(LONG_BUCKET_MS);
    let long: Vec<Cell> = (long_k + 1 - LONG_BUCKETS as i64..=long_k)
        .map(|k| {
            if k == k0 - 5 {
                cell(0.9, 600_000.0, 600_000.0)
            } else {
                cell(0.3, 300_000.0, 0.0)
            }
        })
        .collect();
    let tiers = Tiers {
        long: &long,
        long_k,
        ..Tiers::default()
    };
    let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
    let hour = cells
        .iter()
        .find(|c| c.slot.age_lo_ms == RING_REACH_MS)
        .expect("the newest hour box");
    assert_eq!(
        (now - hour.slot.age_hi_ms, now - hour.slot.age_lo_ms),
        (
            (k0 - 5) * LONG_BUCKET_MS + LONG_BUCKET_MS / 2,
            (k0 + 1) * LONG_BUCKET_MS + LONG_BUCKET_MS / 2
        ),
        "CONTROL: the box spans half of its edge buckets"
    );
    let got = hour.cell;
    assert!((got.tau_ms - 1_950_000.0).abs() < 1e-6, "{got:?}");
    assert!((got.over_ms - 300_000.0).abs() < 1e-6, "{got:?}");
    assert!((got.over_share() - 60.0 / 390.0).abs() < 1e-12, "{got:?}");
    assert!((got.mean.unwrap() - 153.0 / 390.0).abs() < 1e-12, "{got:?}");
    assert_eq!(got.max, Some(0.9));
    assert!(
        (got.mean.unwrap() - 0.35).abs() > 0.02 && (got.over_share() - 120.0 / 390.0).abs() > 0.1,
        "CONTROL: the count and the part each move the box"
    );
}

/// A1 §3's weights: from the 1 s band to the 1 h band each band's boxes weigh strictly more
/// than the finer band's, in every preset, so with gates and cumulative rounding no box of a
/// coarser band lays narrower than one of a finer band; the live box weighs more than a 1 s
/// box. FULL_6H weighs 10; 4, 5, 6, 7, 9 (205 in all). CONTROL: the presets main laid before
/// A1 (FOLD_4H's 2; 1, 1, 1, 2, 2) do not rise.
#[test]
fn band_weights_rise_strictly_from_the_one_second_band_to_the_oldest_in_every_preset() {
    let rises = |bands: &[ScaleBand]| {
        let history: Vec<&ScaleBand> = bands.iter().filter(|b| !b.live_now).collect();
        let live = bands.iter().find(|b| b.live_now);
        history.windows(2).all(|w| w[0].weight < w[1].weight)
            && live
                .zip(history.first())
                .is_some_and(|(l, f)| l.weight > f.weight)
    };
    for spec in [
        MultiScaleSpec::FULL_6H,
        MultiScaleSpec::FOLD_4H,
        MultiScaleSpec::COMPACT_6M,
        MultiScaleSpec::MINI_1M,
    ] {
        assert!(rises(spec.bands), "{:?}", spec.bands);
        assert!(
            spec.bands
                .windows(2)
                .all(|w| w[0].period_ms <= w[1].period_ms),
            "bands run from now outward"
        );
    }
    assert_eq!(MultiScaleSpec::FULL_6H.total_weight(), 205);
    let weights = |spec: MultiScaleSpec| spec.bands.iter().map(|b| b.weight).collect::<Vec<_>>();
    assert_eq!(weights(MultiScaleSpec::FULL_6H), [10, 4, 5, 6, 7, 9]);
    assert_eq!(weights(MultiScaleSpec::FOLD_4H), [4, 1, 2, 3, 4, 5]);
    assert_eq!(weights(MultiScaleSpec::COMPACT_6M), [7, 4, 6, 8]);
    assert_eq!(weights(MultiScaleSpec::MINI_1M), [5, 2, 7]);
    let before = [
        (1, 2, true),
        (9, 1, false),
        (5, 1, false),
        (9, 1, false),
        (5, 2, false),
    ]
    .map(|(slots, weight, live_now)| ScaleBand {
        slots,
        period_ms: 1_000,
        weight,
        live_now,
    });
    assert!(!rises(&before), "CONTROL: equal weights do not rise");
}

/// Ruling 8 (A1 §3): over is a proportional share at every horizon. A four-minute over spell,
/// read once a second, shows in every box that holds any of it while it ages from the 1 min
/// band through the 10 min band into the hour band, each box's share its over time over its
/// read time, so in an hour box it is about 4/60 and never rounds away. CONTROL: under the
/// half-the-readings flag a bucket or box is over only where half its readings were, so the
/// hour box holding the spell would read not over.
#[test]
fn an_over_spell_shows_in_every_box_that_holds_it_at_every_horizon() {
    let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
    let (spell_lo, spell_hi) = (t0 + 2 * 3_600_000, t0 + 2 * 3_600_000 + 240_000);
    let mut horizons = Vec::new();
    for age in [120_000_i64, 1_800_000, 3 * 3_600_000 - 240_000] {
        let now = spell_hi + age;
        let mut r = BucketRing::default();
        for at in (t0..=now).step_by(1_000) {
            let over = (spell_lo..spell_hi).contains(&at);
            let _ = r.push(tk(at), Some(if over { 0.99 } else { 0.3 }), over);
        }
        let (ring, long, recent) = (r.cells(), r.long_cells(), r.readings());
        let tiers = Tiers {
            ring: &ring,
            ring_k: r.open_k().unwrap_or(0),
            long: &long,
            long_k: r.long_k().unwrap_or(0),
            recent: &recent,
        };
        let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
        let holding: Vec<&ScaleCell> = cells
            .iter()
            .filter(|c| {
                let (lo, hi) = (now - c.slot.age_hi_ms, now - c.slot.age_lo_ms);
                lo < spell_hi && hi > spell_lo
            })
            .collect();
        assert!(!holding.is_empty(), "{age} ms on: a box holds the spell");
        for c in &holding {
            let (lo, hi) = (now - c.slot.age_hi_ms, now - c.slot.age_lo_ms);
            // A box past the last minute and inside the hour reads the whole 10 s buckets
            // it touches; an hour box the part of each ten-minute bucket it spans.
            let (lo_r, hi_r) = if c.slot.age_lo_ms >= RING_REACH_MS {
                (lo, hi)
            } else {
                (
                    lo.div_euclid(BUCKET_MS) * BUCKET_MS,
                    ((hi - 1).div_euclid(BUCKET_MS) + 1) * BUCKET_MS,
                )
            };
            let inside = (hi_r.min(spell_hi) - lo_r.max(spell_lo)).max(0) as f64;
            let want = inside / (hi_r - lo_r) as f64;
            let got = c.cell.over_share();
            assert!(got > 0.0, "{age} ms on: {c:?} shows the spell");
            let period = c.slot.age_hi_ms - c.slot.age_lo_ms;
            assert!(
                (got - want).abs() <= 1e-3,
                "{age} ms on: {got} against {want}: {c:?}"
            );
            if period >= 3_600_000 {
                // CONTROL: what main drew before A1. An hour box was over where one of its
                // ten-minute buckets was over for half its readings, and the spell is 4 of
                // its bucket's 10 minutes.
                let first = r.long_k().unwrap_or(0) + 1 - long.len() as i64;
                let half_rule = (lo.div_euclid(LONG_BUCKET_MS)
                    ..=(hi - 1).div_euclid(LONG_BUCKET_MS))
                    .filter_map(|k| long.get(usize::try_from(k - first).ok()?))
                    .any(|b| b.tau_ms > 0.0 && b.over_ms * 2.0 >= b.tau_ms);
                assert!(
                    !half_rule,
                    "CONTROL: the half rule erases the spell from {c:?}"
                );
            }
        }
        horizons.push(
            holding
                .iter()
                .map(|c| c.slot.age_hi_ms - c.slot.age_lo_ms)
                .max(),
        );
    }
    assert_eq!(
        horizons,
        [Some(60_000), Some(600_000), Some(3_600_000)],
        "CONTROL: the spell was seen in the 1 min, 10 min and 1 h bands"
    );
}

/// Record §5.2: weights are time, not readings. A minute read half at the focused cadence
/// (1.0, a reading a second) and half in the background (0.0, a reading each 5 s), each on
/// its half-slot, reads 0.5 in its 1 min box, because the strip's x is time and each half is
/// 30 s of it. CONTROL: counting readings, its 30 readings at 1.0 and 6 at 0.0 read 0.83.
#[test]
fn coarse_boxes_weigh_their_buckets_by_time_not_by_reading_count() {
    let mut r = BucketRing::default();
    for i in 0..30 {
        let _ = r.push(Taken::at(T0 + i * 1_000 + 500, 1_000), Some(1.0), false);
    }
    for j in 0..6 {
        let _ = r.push(
            Taken::at(T0 + 30_000 + j * 5_000 + 2_500, 5_000),
            Some(0.0),
            false,
        );
    }
    let _ = r.push(Taken::at(T0 + 60_000 + 2_500, 5_000), Some(0.0), false);
    let now = T0 + 120_000;
    let (ring, recent) = (r.cells(), r.readings());
    let tiers = Tiers {
        ring: &ring,
        ring_k: r.open_k().unwrap_or(0),
        recent: &recent,
        ..Tiers::default()
    };
    let cells = multi_scale_cells(tiers, now, MultiScaleSpec::FULL_6H, 2_500, &[]);
    let minute = cells
        .iter()
        .find(|c| c.slot.age_lo_ms == 60_000 && c.slot.age_hi_ms == 120_000)
        .expect("the 1 min band's newest box");
    assert_eq!(minute.cell.tau_ms, 60_000.0, "every ms read once");
    assert!(
        (minute.cell.mean.expect("read") - 0.5).abs() < 1e-12,
        "{minute:?}"
    );
    let (n, sum) = r
        .iter()
        .filter(|(k, _)| (T0 / BUCKET_MS..T0 / BUCKET_MS + 6).contains(k))
        .fold((0.0, 0.0), |(n, sum), (_, b)| {
            (n + f64::from(b.n), sum + f64::from(b.n) * b.mean)
        });
    assert!(
        (sum / n - 30.0 / 36.0).abs() < 1e-12,
        "CONTROL: {}",
        sum / n
    );
}

/// Record §5.2: the 1 h box leaving the 6 h horizon still has its children. An hour box's
/// epoch can stand an hour past the sixth (A1 §6.2's pour steps the hour bands once an
/// hour), so the ring keeps every ten-minute bucket of the seven hours before now, and the
/// open one. Negative control: `LONG_BUCKETS` 37, as main kept, fails the leaving hour's
/// first bucket (the mutant the F4′ review ran); the scenario's leaving bucket is one 37
/// buckets would not hold.
#[test]
fn the_horizons_leaving_box_keeps_its_children() {
    let mut r = BucketRing::default();
    let t0 = T0 - T0.rem_euclid(LONG_BUCKET_MS);
    let mut now = t0;
    for i in 0..9 * 360 {
        now = t0 + i * 10_000 + 5_000;
        let _ = r.push(Taken::at(now, 10_000), Some(1.0), false);
    }
    let long = r.long_cells();
    let long_k = r.long_k().expect("read");
    let first = long_k + 1 - long.len() as i64;
    let leaving = (now - 7 * 3_600_000).div_euclid(LONG_BUCKET_MS);
    for k in leaving..=(now - 6 * 3_600_000).div_euclid(LONG_BUCKET_MS) {
        let cell = usize::try_from(k - first)
            .ok()
            .and_then(|i| long.get(i))
            .copied()
            .unwrap_or_default();
        assert_eq!(cell.mean, Some(1.0), "bucket {k} of the leaving hour");
    }
    assert!(
        long_k + 1 - 37 > leaving,
        "the scenario: 37 buckets would start after the leaving one"
    );
}

/// Record §5.2: a reading late past its slot is never counted twice in a coarse box. Each
/// reading weighs only the part of its slot that no earlier reading stands for, so however
/// the readings jitter a bucket's read time is the union of their slots inside it, and never
/// more than the bucket; a late reading still reaches the strip's finest boxes, with the time
/// it stands for. (Its borrow of the slot it missed is the display's, `SLOT_COVER_MS`, A1's
/// R7.) CONTROL: summing each reading's whole slot counts the jittered time twice.
#[test]
fn a_late_reading_borrows_for_display_and_counts_once() {
    let jitter = [0, 400, 950, 300, -200, 0, 700, 100, 0, 300];
    let times: Vec<i64> = jitter
        .iter()
        .enumerate()
        .map(|(i, j)| T0 + i as i64 * 1_000 + 500 + j)
        .collect();
    let mut r = BucketRing::default();
    for &t in &times {
        let _ = r.push(tk(t), Some(1.0), false);
    }
    let b = *r.bucket(T0 / BUCKET_MS).expect("read");
    let mut union: Vec<(i64, i64)> = times
        .iter()
        .map(|&t| tk(t).slot())
        .map(|(lo, hi)| (lo.max(T0), hi.min(T0 + BUCKET_MS)))
        .collect();
    union.sort_unstable();
    let (mut covered, mut to) = (0, i64::MIN);
    for (lo, hi) in union {
        covered += (hi - lo.max(to)).max(0);
        to = to.max(hi);
    }
    assert_eq!(i64::from(b.tau_ms), covered);
    assert!(b.tau_ms <= 10_000);
    assert_eq!(b.n, 10);
    let late = r
        .readings()
        .into_iter()
        .find(|x| x.at_ms == T0 + 2_000 + 500 + 950)
        .expect("the late reading reaches the finest boxes");
    assert!(late.w_ms >= 1);
    let naive: i64 = times
        .iter()
        .map(|&t| tk(t).slot())
        .map(|(lo, hi)| hi.min(T0 + BUCKET_MS) - lo.max(T0))
        .sum();
    assert!(naive > covered, "CONTROL: {naive} against {covered}");
}
