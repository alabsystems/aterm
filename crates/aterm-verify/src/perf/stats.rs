// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Summary statistics, nearest-rank tails, and order-balanced ABBA round intervals.

use aterm_json::{Value, json};

/// The median of `values` (`NaN` for an empty slice).
pub(crate) fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) / 2.0
    }
}

pub(crate) fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// The nearest-rank percentile (`p` in 0..=1) of a non-empty slice.
pub(crate) fn percentile_nearest_rank(values: &[f64], p: f64) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = (p * v.len() as f64).ceil() as usize;
    v[rank.saturating_sub(1).min(v.len() - 1)]
}

/// Minimum sample count required to report a p95 instead of the maximum.
pub(crate) const P95_MIN_N: usize = 20;

/// `median=… p95_nearest_rank=…` or `max=… (p95 omitted…)`, for a summary line.
pub(crate) fn tail_summary(values: &[f64]) -> String {
    if values.len() < P95_MIN_N {
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        format!("max={max:.2} (p95 omitted: needs >={P95_MIN_N} samples)")
    } else {
        format!(
            "p95_nearest_rank={:.2}",
            percentile_nearest_rank(values, 0.95)
        )
    }
}

/// One arm's distribution, as the JSON results record it.
pub(crate) fn distribution(values: &[f64]) -> Value {
    json!({
        "values": values,
        "median": median(values),
        "p95_nearest_rank": (values.len() >= P95_MIN_N).then(|| percentile_nearest_rank(values, 0.95)),
        "min": values.iter().copied().fold(f64::INFINITY, f64::min),
        "max": values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    })
}

/// Folds consecutive AB/BA pair deltas into order-balanced ABBA round deltas.
pub(crate) fn fold_rounds(deltas: &[f64]) -> Vec<f64> {
    deltas
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[a, b]| (a + b) / 2.0)
        .collect()
}

/// Distribution-free symmetric order-statistic interval for the median (>= 95% coverage).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MedianInterval {
    pub low: f64,
    pub high: f64,
    pub coverage: f64,
}

pub(crate) fn median_sign_interval(values: &[f64]) -> Option<MedianInterval> {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return None;
    }
    // P(Bin(n, 1/2) = k) by recurrence, so no factorial overflows.
    let mut pmf = Vec::with_capacity(n + 1);
    let mut p = 0.5f64.powi(n as i32);
    for k in 0..=n {
        pmf.push(p);
        p *= (n - k) as f64 / (k + 1) as f64;
    }
    for rank in (0..=(n - 1) / 2).rev() {
        // Coverage of [x_(rank+1), x_(n-rank)] is 1 - 2 P(Bin <= rank).
        let tail: f64 = pmf[..=rank].iter().sum();
        let coverage = 1.0 - 2.0 * tail;
        if coverage >= 0.95 {
            return Some(MedianInterval {
                low: v[rank],
                high: v[n - 1 - rank],
                coverage,
            });
        }
    }
    None
}

/// Two-sided 0.975 Student-t quantiles by degrees of freedom (1..=30); beyond
/// the table the normal quantile is within one percent.
const T_QUANTILE_975: [f64; 30] = [
    12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
    2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
    2.052, 2.048, 2.045, 2.042,
];

/// A 95% Student-t interval on the mean.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MeanInterval {
    pub low: f64,
    pub high: f64,
    pub half_width: f64,
}

/// Computes the 95% Student-t interval on the mean of `values` (`None` below two values).
pub(crate) fn mean_interval(values: &[f64]) -> Option<MeanInterval> {
    let n = values.len();
    if n < 2 {
        return None;
    }
    let centre = mean(values);
    let var = values.iter().map(|v| (v - centre).powi(2)).sum::<f64>() / (n - 1) as f64;
    let t = T_QUANTILE_975.get(n - 2).copied().unwrap_or(1.960);
    let half = t * var.sqrt() / (n as f64).sqrt();
    Some(MeanInterval {
        low: centre - half,
        high: centre + half,
        half_width: half,
    })
}

/// Summary over order-balanced ABBA round deltas and their intervals.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rounds {
    pub values: Vec<f64>,
    pub median: f64,
    pub mean: f64,
    pub faster: usize,
    pub ties: usize,
    pub slower: usize,
    pub median_interval: Option<MedianInterval>,
    pub mean_interval: Option<MeanInterval>,
}

/// [`Rounds`] folded from pair deltas; `None` when no round formed.
pub(crate) fn rounds(deltas: &[f64]) -> Option<Rounds> {
    let values = fold_rounds(deltas);
    if values.is_empty() {
        return None;
    }
    Some(Rounds {
        median: median(&values),
        mean: mean(&values),
        faster: values.iter().filter(|v| **v > 0.0).count(),
        ties: values.iter().filter(|v| **v == 0.0).count(),
        slower: values.iter().filter(|v| **v < 0.0).count(),
        median_interval: median_sign_interval(&values),
        mean_interval: mean_interval(&values),
        values,
    })
}

impl Rounds {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "values": self.values,
            "rounds": self.values.len(),
            "median": self.median,
            "mean": self.mean,
            "candidate_faster_rounds": self.faster,
            "ties": self.ties,
            "candidate_slower_rounds": self.slower,
            "median_interval": self.median_interval.map(|i| json!({
                "low": i.low, "high": i.high, "coverage": i.coverage
            })),
            "mean_interval": self.mean_interval.map(|i| json!({
                "low": i.low, "high": i.high, "half_width": i.half_width
            })),
            "resolution_ms": self.mean_interval.map(|i| i.half_width),
        })
    }
}

/// Formats per-pair deltas as JSON, marked `order_contaminated`.
pub(crate) fn paired_json(deltas: &[f64]) -> Value {
    let mut v = distribution(deltas);
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "candidate_faster_pairs".into(),
            json!(deltas.iter().filter(|d| **d > 0.0).count()),
        );
        o.insert(
            "ties".into(),
            json!(deltas.iter().filter(|d| **d == 0.0).count()),
        );
        o.insert(
            "candidate_slower_pairs".into(),
            json!(deltas.iter().filter(|d| **d < 0.0).count()),
        );
        o.insert("order_contaminated".into(), Value::Bool(true));
    }
    v
}

#[cfg(test)]
mod tests {
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
}
