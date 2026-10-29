// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The distribution summary ([`crate::stats`]), checked; mounted only by the ordinary
//! fabric_bench test target.

use aterm_json::Value;

use crate::stats::*;

#[test]
fn quantiles_are_nearest_rank_values_that_were_measured() {
    let values: Vec<f64> = (1..=100).map(f64::from).collect();
    let s = summarize(&values).expect("non-empty");
    assert_eq!(s.n, 100);
    assert_eq!((s.p50, s.p95, s.p99), (50.0, 95.0, 99.0));
    assert_eq!((s.min, s.max), (1.0, 100.0));
    assert!((s.mean - 50.5).abs() < 1e-9);
    let s = summarize(&[3.0, 1.0, 2.0]).expect("non-empty");
    assert_eq!((s.p50, s.p95, s.p99), (2.0, 3.0, 3.0));
    let s = summarize(&[7.0]).expect("non-empty");
    assert_eq!((s.p50, s.p99), (7.0, 7.0));
    assert!(summarize(&[]).is_none());
}

#[test]
fn the_json_carries_the_summary_beside_the_raw_values() {
    let v = to_json(&[1.0004, 2.0]);
    assert_eq!(v.get("n").and_then(Value::as_u64), Some(2));
    assert_eq!(v.get("p99").and_then(Value::as_f64), Some(2.0));
    assert_eq!(
        v.get("values").and_then(Value::as_array).map(|a| a.len()),
        Some(2)
    );
    assert_eq!(to_json(&[]), Value::Null);
    assert_eq!(row("x", &[]), "| x | 0 | – | – | – | – |");
    assert_eq!(
        row("x", &[2.0, 4.0]),
        "| x | 2 | 3.00 | 2.00 | 4.00 | 4.00 |"
    );
}
