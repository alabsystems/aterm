// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The package lane's local release wake after a timed-out store-lock wait.

use super::*;

/// One contention-park slice. A live writer keeps the lane parked; the first
/// slice that observes its release retries the same owed pass. A bump or ready
/// vendor answer wins that slice, and a park without a timed-out child never
/// probes the store lock. Buggy=1 both retries while held and misses a
/// released writer, with independent counterexamples for the two latency and
/// resource properties. Tier-1 binds Check to the real read-only flock probe
/// and the GUI's park-end decision.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_contention_release_park_model() -> Model {
    crate::ty_model! {
        AtpkgContentionReleasePark {
            const Buggy = 0;
            // 1 while the other writer holds its exclusive store flock.
            var held = 1;
            // Only a contention park asks the local lock question.
            var enabled = 1;
            // 0 none, 1 bump, 2 vendor.
            var higher = 0;
            var checked = 0;
            // 0 keep parking, 1 bump, 2 vendor, 3 retry after release.
            var decision = 0;

            action Release when (held == 1 && checked == 0) {
                held = 0;
            }
            action Disable when (enabled == 1 && checked == 0) {
                enabled = 0;
            }
            action OfferBump when (higher == 0 && checked == 0) {
                higher = 1;
            }
            action OfferVendor when (higher == 0 && checked == 0) {
                higher = 2;
            }
            action Check when (checked == 0) {
                checked = 1;
                decision = if higher > 0 {
                    if Buggy == 1 { 3 } else { higher }
                } else if enabled == 1 && held == 0 {
                    if Buggy == 1 { 0 } else { 3 }
                } else if Buggy == 1 && held == 1 {
                    3
                } else {
                    0
                };
            }
            action NextSlice when (checked == 1 && decision == 0) {
                checked = 0;
            }

            invariant HeldWriterKeepsParked: held == 0 || decision <= 2;
            invariant ReleasedWriterWakesOnCheck:
                held == 1 || enabled == 0 || higher > 0 || checked == 0 || decision == 3;
            invariant HigherAnswerWins:
                higher == 0 || checked == 0 || decision == higher;
            invariant DisabledParkNeverProbes:
                enabled == 1 || checked == 0 || decision <= 2;
        }
    }
}
