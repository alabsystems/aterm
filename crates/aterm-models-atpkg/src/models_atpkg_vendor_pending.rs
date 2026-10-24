// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded independent vendor-head checks and exactly-once result harvest.

use aterm_spec::derive::*;

/// Model of the two vendor-head slots moving from idle to in-flight, ready, and offered or retried.
/// `Buggy = 1` permits one fault (`fault_used`), which keeps the mutant's state space finite for
/// the non-vacuity sweep.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_vendor_pending_check_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgVendorPendingCheck {
            const Buggy = 0;
            var first = 0;
            var second = 0;
            var first_answer = 0;
            var second_answer = 0;
            var active = 0;
            var completed = 0;
            var harvested = 0;
            var fault_used = 0;

            action StartFirst when (first == 0 && second == 0) {
                first = 1;
                active = active + 1;
            }
            action StartSecond when ((first == 1 || first == 2) && second == 0) {
                second = 1;
                active = active + 1;
            }
            action FirstReady when (first == 1) {
                first = 2;
                first_answer = 1;
                active = active - 1;
                completed = completed + 1;
            }
            action FirstFailed when (first == 1) {
                first = 2;
                first_answer = 2;
                active = active - 1;
                completed = completed + 1;
            }
            action SecondReady when (second == 1) {
                second = 2;
                second_answer = 1;
                active = active - 1;
                completed = completed + 1;
            }
            action SecondFailed when (second == 1) {
                second = 2;
                second_answer = 2;
                active = active - 1;
                completed = completed + 1;
            }
            action OfferFirst when (first == 2 && first_answer == 1) {
                first = 3;
                harvested = harvested + 1;
            }
            action RetryFirst when (first == 2 && first_answer == 2) {
                first = 4;
                harvested = harvested + 1;
            }
            action OfferSecond when (second == 2 && second_answer == 1) {
                second = 3;
                harvested = harvested + 1;
            }
            action RetrySecond when (second == 2 && second_answer == 2) {
                second = 4;
                harvested = harvested + 1;
            }

            action SpawnExtra when (Buggy == 1 && fault_used == 0 && first == 1 && second == 1) {
                active = active + 1;
                fault_used = 1;
            }
            action CompleteTwice when (Buggy == 1 && fault_used == 0 && first == 2) {
                completed = completed + 1;
                fault_used = 1;
            }
            action OfferTwice when (Buggy == 1 && fault_used == 0 && first == 3) {
                harvested = harvested + 1;
                fault_used = 1;
            }
            action DropFirstReady when (Buggy == 1 && fault_used == 0 && first == 2) {
                first = 0;
                fault_used = 1;
            }
            action LoseFirstRetry when (Buggy == 1 && fault_used == 0 && first == 2 && first_answer == 2) {
                first = 3;
                harvested = harvested + 1;
                fault_used = 1;
            }

            invariant TwoActiveChecks: active <= 2;
            invariant ActiveMatchesSlots:
                active == (if first == 1 { 1 } else { 0 }) +
                    (if second == 1 { 1 } else { 0 });
            invariant OneCompletionPerSlot:
                completed == (if first > 1 { 1 } else { 0 }) +
                    (if second > 1 { 1 } else { 0 });
            invariant EachCompletionConsumedOnce:
                harvested == (if first > 2 { 1 } else { 0 }) +
                    (if second > 2 { 1 } else { 0 });
            invariant OutstandingReadyIsOwned:
                completed == harvested + (if first == 2 { 1 } else { 0 }) +
                    (if second == 2 { 1 } else { 0 });
            invariant FailedHeadHasRetry:
                (first == 4 || first_answer <= 1 || first <= 2) &&
                (second == 4 || second_answer <= 1 || second <= 2);
        }
    }
}
