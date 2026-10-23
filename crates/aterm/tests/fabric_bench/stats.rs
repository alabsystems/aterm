// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nearest-rank quantile and mean distribution summary.

use aterm_json::{Value, json};

/// A distribution, summarised.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub min: f64,
    pub max: f64,
}

/// The value at nearest rank `p` (0..=1) of an ascending, non-empty slice.
fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    let hundredths = (p * 100.0).round() as usize;
    let rank = (hundredths * n).div_ceil(100).clamp(1, n);
    sorted[rank - 1]
}

/// `values` summarised, or `None` when there are none.
#[must_use]
pub fn summarize(values: &[f64]) -> Option<Summary> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(Summary {
        n: sorted.len(),
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
        p50: nearest_rank(&sorted, 0.50),
        p95: nearest_rank(&sorted, 0.95),
        p99: nearest_rank(&sorted, 0.99),
        min: sorted[0],
        max: sorted[sorted.len() - 1],
    })
}

/// Round to what a millisecond figure can honestly carry here.
#[must_use]
pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// `{n, mean, p50, p95, p99, min, max, values}`, or `null` for no values.
#[must_use]
pub fn to_json(values: &[f64]) -> Value {
    match summarize(values) {
        None => Value::Null,
        Some(s) => json!({
            "n": s.n,
            "mean": round3(s.mean),
            "p50": round3(s.p50),
            "p95": round3(s.p95),
            "p99": round3(s.p99),
            "min": round3(s.min),
            "max": round3(s.max),
            "values": values.iter().map(|v| round3(*v)).collect::<Vec<f64>>(),
        }),
    }
}

/// One table row: `| what | n | mean | p50 | p95 | p99 |`.
#[must_use]
pub fn row(what: &str, values: &[f64]) -> String {
    match summarize(values) {
        None => format!("| {what} | 0 | – | – | – | – |"),
        Some(s) => format!(
            "| {what} | {} | {:.2} | {:.2} | {:.2} | {:.2} |",
            s.n, s.mean, s.p50, s.p95, s.p99
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quantiles_are_nearest_rank_values_that_were_measured() {
        use super::*;

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
        use super::*;

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
}
