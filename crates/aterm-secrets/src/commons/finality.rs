// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When a checkpoint is final: the one rule the vault log (docs/DESIGN-vault-log-2026-11-14.md
//! step 4, FL-148) and the Commons table store (docs/DESIGN-table-checkpoint-2026-11-15.md §4,
//! LT-B10B) share, so both stores call this and neither restates it.
//!
//! A checkpoint at a cut is final when more than half of the seats at the cut that signed a
//! record in the [`HEARD_WITHIN_MS`] before the cut have each signed a checkpoint with the same
//! body. Counting only the seats heard recently, not every seat, is what lets a store whose dead
//! seats outnumber its live ones checkpoint at all (KI-1113-1); the cost, stated in both designs,
//! is that a newcomer that trusts the shares can be shown a state a hostile majority of recent
//! seats certified. A seat that holds the cut's past recomputes the body and is not exposed.

use std::collections::BTreeSet;

/// How long before a cut a seat must have signed a record to be counted: 30 days.
pub const HEARD_WITHIN_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// The seats of `seats` that signed a record within [`HEARD_WITHIN_MS`] before `cut_ms`, by
/// `last_signed_ms`, each seat's newest signed record at or before the cut (`None` for a seat
/// that signed none).
pub fn recent<A: Ord + Clone>(
    seats: &BTreeSet<A>,
    cut_ms: u64,
    last_signed_ms: impl Fn(&A) -> Option<u64>,
) -> BTreeSet<A> {
    let since = cut_ms.saturating_sub(HEARD_WITHIN_MS);
    seats
        .iter()
        .filter(|seat| last_signed_ms(seat).is_some_and(|at| at >= since && at <= cut_ms))
        .cloned()
        .collect()
}

/// Whether the shares `signed` (the seats that signed one identical checkpoint body) make it
/// final among the `recent` seats: more than half of them. No recent seat means no checkpoint is
/// final: a store nobody has written to for 30 days has nobody to certify it.
pub fn is_final<A: Ord>(recent: &BTreeSet<A>, signed: &BTreeSet<A>) -> bool {
    let shares = recent.intersection(signed).count();
    !recent.is_empty() && shares * 2 > recent.len()
}

#[cfg(test)]
#[path = "finality_tests.rs"]
mod tests;
