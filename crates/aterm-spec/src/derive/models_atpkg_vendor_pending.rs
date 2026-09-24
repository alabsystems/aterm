// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded independent vendor-head checks and exactly-once result harvest.

use super::*;

/// Each of the two vendor-head slots moves from idle, to in flight, to ready,
/// then to either an offered head or a scheduled retry. A ready result may be
/// harvested while the other vendor is still in flight: the slow head must not
/// hold the fast head's update. At most two checks may be active, and a slot's
/// completion is consumed exactly once. Tier-1 drives the real pending watch
/// through both completion orders and a failed-head retry.
///
/// `Buggy=1` permits one fault injection: an extra worker, a duplicate
/// completion or harvest, a dropped ready result, or discarding a failed head
/// without retry. Each has a counterexample. The one-fault bound keeps the interpreter's
/// per-invariant non-vacuity sweep finite even when it disables the other
/// invariants while searching for a counterexample.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_vendor_pending_check_model() -> Model {
    crate::ty_model! {
        AtpkgVendorPendingCheck {
            const Buggy = 0;
            // 0 idle, 1 in flight, 2 ready, 3 offered, 4 retry scheduled.
            var first = 0;
            var second = 0;
            // 0 no answer, 1 newer head, 2 unreachable/retryable failure.
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
