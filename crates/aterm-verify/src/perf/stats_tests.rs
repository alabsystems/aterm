// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn median_and_nearest_rank() {
    assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
    assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    assert!(median(&[]).is_nan());
    let v: Vec<f64> = (1..=20).map(f64::from).collect();
    assert_eq!(percentile_nearest_rank(&v, 0.95), 19.0);
    assert_eq!(percentile_nearest_rank(&[7.0], 0.95), 7.0);
    assert!(tail_summary(&[1.0, 2.0]).starts_with("max=2.00"));
    assert!(tail_summary(&v).starts_with("p95_nearest_rank=19.00"));
}

#[test]
fn rounds_fold_consecutive_pairs_and_drop_a_trailing_one() {
    assert_eq!(fold_rounds(&[-2.0, 2.0, -2.0, 2.0]), vec![0.0, 0.0]);
    assert_eq!(fold_rounds(&[1.0, 3.0]), vec![2.0]);
    assert_eq!(fold_rounds(&[1.0, 3.0, 9.0]), vec![2.0]);
    assert!(fold_rounds(&[1.0]).is_empty());
    assert!(fold_rounds(&[]).is_empty());
    assert!(rounds(&[5.0]).is_none());
}

/// The INCLUSIVE binomial tail: an off-by-one would silently narrow every
/// interval the lane prints.
#[test]
fn the_median_interval_has_textbook_coverage() {
    let values: Vec<f64> = (0..10).map(f64::from).collect();
    let i = median_sign_interval(&values).expect("n=10 has one");
    assert_eq!((i.low, i.high), (1.0, 8.0));
    let expected = 1.0 - 2.0 * (1.0 + 10.0) / 1024.0;
    assert!((i.coverage - expected).abs() < 1e-12, "{i:?}");
    assert!(median_sign_interval(&[0.0, 1.0, 2.0, 3.0, 4.0]).is_none());
}

#[test]
fn identical_rounds_resolve_nothing_they_have_not_earned() {
    let r = rounds(&[-2.0, 2.0, -2.0, 2.0]).expect("two rounds");
    assert_eq!(r.values, vec![0.0, 0.0]);
    assert_eq!((r.faster, r.ties, r.slower), (0, 2, 0));
    assert_eq!(
        r.mean_interval,
        Some(MeanInterval {
            low: 0.0,
            high: 0.0,
            half_width: 0.0
        })
    );
    assert!(r.median_interval.is_none());
}

#[test]
fn the_t_interval_uses_the_quantile_for_its_degrees_of_freedom() {
    let i = mean_interval(&[1.0, 2.0, 3.0]).expect("n=3");
    assert!((i.half_width - 4.303 / 3f64.sqrt()).abs() < 1e-9, "{i:?}");
    assert!(mean_interval(&[1.0]).is_none());
}
