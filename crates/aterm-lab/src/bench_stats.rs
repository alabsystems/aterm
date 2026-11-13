// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the lab's pre-registered benches share: the Williams order, a session's valid rounds, the
//! median, the percentiles and the wall-time span, and the machine's load average.

use std::time::Duration;

/// A Williams square for an even `n`: `n` orders of `0..n`, each value once in every position and
/// immediately after every other value exactly once. Row 0 is 0, 1, n−1, 2, n−2, …; row r adds r
/// mod n. An odd `n` has no such square of `n` rows: `None`. Pure.
#[must_use]
pub fn williams(n: usize) -> Option<Vec<Vec<usize>>> {
    if n == 0 || !n.is_multiple_of(2) {
        return None;
    }
    let mut first = vec![0];
    let (mut lo, mut hi) = (0, n);
    while first.len() < n {
        if first.len() % 2 == 1 {
            lo += 1;
            first.push(lo);
        } else {
            hi -= 1;
            first.push(hi);
        }
    }
    Some(
        (0..n)
            .map(|r| first.iter().map(|v| (v + r) % n).collect())
            .collect(),
    )
}

/// A session's valid rounds of `rounds`: 10 of the registered 12, `N − ⌊N/6⌋` of a smoke run's N.
#[must_use]
pub const fn required_valid(rounds: usize) -> usize {
    rounds - rounds / 6
}

/// The median: the middle value, or the mean of the two middle values. Pure.
#[must_use]
pub fn median(xs: &[f64]) -> Option<f64> {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => v.get(n / 2).copied(),
        _ => Some((v.get(n / 2 - 1)? + v.get(n / 2)?) / 2.0),
    }
}

/// A series' wall-time span over the valid rounds: max / min. Pure.
#[must_use]
pub fn span_ratio(walls: &[f64]) -> Option<f64> {
    let lo = walls.iter().copied().min_by(f64::total_cmp)?;
    let hi = walls.iter().copied().max_by(f64::total_cmp)?;
    (lo > 0.0).then_some(hi / lo)
}

/// The 1-minute load average of `sysctl -n vm.loadavg`'s `{ 1m 5m 15m }`. Pure.
#[must_use]
fn parse_loadavg(text: &str) -> Option<f64> {
    text.split_whitespace()
        .find(|w| *w != "{")
        .and_then(|w| w.parse().ok())
}

/// The 1-minute load average, from `sysctl -n vm.loadavg` spawned through `spawnfd`.
pub(crate) fn load_average() -> Result<f64, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/sysctl");
    cmd.args(["-n", "vm.loadavg"]);
    let out = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(10))
        .map_err(|e| format!("sysctl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!("sysctl exited {}", out.status));
    }
    parse_loadavg(&text).ok_or_else(|| format!("sysctl printed `{}`", text.trim()))
}

/// The 50th, 95th and 99th percentiles of `xs` nanoseconds, in milliseconds, by nearest rank. No
/// values: `None`. Pure.
#[must_use]
pub fn percentiles(xs: &[u64]) -> Option<(f64, f64, f64)> {
    let mut v = xs.to_vec();
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let at = |p: usize| {
        let rank = (p * v.len()).div_ceil(100).max(1);
        let ns = v.get(rank - 1).copied().unwrap_or(0);
        ns as f64 / 1e6
    };
    Some((at(50), at(95), at(99)))
}

#[cfg(test)]
#[path = "bench_stats_tests.rs"]
mod tests;
