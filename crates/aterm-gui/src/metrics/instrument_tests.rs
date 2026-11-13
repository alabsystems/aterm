// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{H_BUCKETS, HistCopy, Histogram, Stamp};

#[test]
fn latency_bucket_resolution() {
    type Fine = super::HistogramBins<{ super::LATENCY_BUCKETS }>;
    // Bound every bucket, not only the values in the control distribution.
    // The final bucket saturates; no bound is claimed above 2^36 ns.
    for idx in 0..super::LATENCY_BUCKETS {
        let lower = if idx == 0 {
            0
        } else {
            Fine::upper_edge(idx - 1)
        };
        let upper = Fine::upper_edge(idx);
        assert_eq!(Fine::index(lower), idx);
        assert_eq!(Fine::index(upper - 1), idx);
        assert!(upper - lower <= (lower / 1024).max(1));
    }
}

/// Every representative value lands in a bucket whose edges contain it,
/// and indices are monotone in the value — across the linear range, every
/// octave boundary, and the saturation clamp.
#[test]
fn bucket_index_is_monotone_and_edges_contain_values() {
    let mut last_idx = 0usize;
    let mut v = 1u64;
    while v < (1u64 << 36) {
        let idx = Histogram::index(v);
        assert!(
            idx >= last_idx,
            "index regressed at v={v}: {idx} < {last_idx}"
        );
        assert!(
            v < Histogram::upper_edge(idx),
            "v={v} not below its bucket's exclusive upper edge {}",
            Histogram::upper_edge(idx)
        );
        if idx > 0 {
            assert!(
                v >= Histogram::upper_edge(idx - 1),
                "v={v} below the previous bucket's exclusive upper edge"
            );
        }
        last_idx = idx;
        v = v.saturating_mul(2) - v / 3;
    }
    assert_eq!(
        Histogram::index(u64::MAX),
        H_BUCKETS - 1,
        "saturates at the top"
    );
}

/// Percentiles on a known distribution: conservative (>= true value) and
/// within one bucket's relative error of it.
#[test]
fn percentiles_are_conservative_and_tight() {
    let h = Histogram::new();
    assert_eq!(
        h.copy_out().percentile(0.5),
        None,
        "empty histogram has no percentile"
    );
    for us in 1..=1000u64 {
        h.record(us * 1000);
    }
    let copy = h.copy_out();
    assert_eq!(copy.n(), 1000);
    for (q, true_ns) in [(0.5, 500_000u64), (0.95, 950_000), (0.99, 990_000)] {
        let got = copy.percentile(q).unwrap();
        assert!(
            got >= true_ns,
            "p{q} {got} below true {true_ns} — not conservative"
        );
        assert!(
            got <= true_ns + true_ns / 4,
            "p{q} {got} more than 25% above true {true_ns} — bucket too coarse"
        );
    }
    let one = Histogram::new();
    one.record(7_777);
    let one = one.copy_out();
    assert_eq!(one.percentile(0.5), one.percentile(1.0));
    assert!(one.percentile(0.0001).unwrap() >= 7_777);
}

/// Whatever bucket vector a copy holds (a racing reset can leave any
/// subset of buckets zeroed), every quantile lands in a populated bucket,
/// because `n` is the sum of exactly those buckets.
#[test]
fn every_quantile_of_a_copy_lies_inside_its_populated_buckets() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..512 {
        let mut counts = [0u64; H_BUCKETS];
        let populated = 1 + (next() % 12) as usize;
        for _ in 0..populated {
            counts[(next() % H_BUCKETS as u64) as usize] += 1 + next() % 1_000;
        }
        let first = counts.iter().position(|&c| c != 0).unwrap();
        let last = counts.iter().rposition(|&c| c != 0).unwrap();
        let copy = HistCopy::from_counts(counts);
        assert_eq!(copy.n(), counts.iter().sum::<u64>());
        for q in [0.01, 0.5, 0.95, 0.99, 1.0] {
            let got = copy.percentile(q).expect("a populated copy has quantiles");
            let idx = (0..H_BUCKETS)
                .find(|&i| Histogram::upper_edge(i) == got)
                .expect("a quantile is a bucket edge");
            assert_ne!(counts[idx], 0, "p{q} landed in an empty bucket");
            assert!(
                (Histogram::upper_edge(first)..=Histogram::upper_edge(last)).contains(&got),
                "p{q}={got} outside buckets {first}..={last}"
            );
        }
    }
    assert_eq!(HistCopy::from_counts([0; H_BUCKETS]).percentile(0.99), None);
    let mut low = [0u64; H_BUCKETS];
    low[0] = 50;
    let low = HistCopy::from_counts(low);
    assert_eq!(low.percentile(0.99), Some(Histogram::upper_edge(0)));
}

/// A reset empties the distribution, and a reset between two samples leaves
/// exactly the survivor: one sample, whose p99 is its own bucket's edge.
#[test]
fn record_reset_record_publishes_the_survivor() {
    let h = Histogram::new();
    h.record(5_000_000);
    h.reset();
    assert_eq!(h.copy_out().n(), 0);
    assert_eq!(h.copy_out().percentile(0.99), None);
    h.record(3_000_000);
    let copy = h.copy_out();
    assert_eq!(copy.n(), 1);
    assert_eq!(
        copy.percentile(0.99),
        Some(Histogram::upper_edge(Histogram::index(3_000_000)))
    );
}

/// A stamp keeps its OLDEST opening until a close takes it, and a close
/// with nothing open takes nothing.
#[test]
fn a_stamp_keeps_the_oldest_opening_until_it_is_taken() {
    let stamp = Stamp::new();
    assert_eq!(stamp.take(), None);
    stamp.arm(10);
    stamp.arm(20);
    assert_eq!(
        stamp.take(),
        Some(10),
        "a burst measures from its first edge"
    );
    assert_eq!(stamp.take(), None, "taken once");
    stamp.arm(30);
    assert_eq!(stamp.take(), Some(30));
}
