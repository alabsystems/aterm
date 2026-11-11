// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact decisions about a predeclared finite sample, without a population claim.
//! Unseen readings range over every `u64`. These are completion bounds, not confidence
//! intervals: optional stopping adds zero error relative to collecting the full cap.

/// The possible values of the `rank`th smallest reading after collecting exactly `cap`.
/// Ranks are one-based. Invalid plans and prefixes return `None`.
#[must_use]
pub fn completion(values: &[u64], cap: usize, rank: usize) -> Option<(u64, u64)> {
    if cap == 0 || rank == 0 || rank > cap || values.len() > cap {
        return None;
    }
    let missing = cap - values.len();
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let lo = rank
        .checked_sub(missing)
        .and_then(|r| r.checked_sub(1))
        .map_or(0, |i| sorted[i]);
    let hi = sorted.get(rank - 1).copied().unwrap_or(u64::MAX);
    Some((lo, hi))
}

#[cfg(test)]
#[path = "decision_tests.rs"]
mod tests;
