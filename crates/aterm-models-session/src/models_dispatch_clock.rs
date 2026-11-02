// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A hardware arrival belongs to its thread and Metrics instance until its dispatch exits.

use aterm_spec::derive::Model;

/// Tokens 1 and 2 belong to outer and inner UI scopes; token 3 is the worker's own clock.
/// Worker observations interleave with scope entry, UI claims and scope exit. Remaining
/// tokens are the UI's ownership expectation, independent of the stored arrival.
/// Mutants expose shared reads, shared claims, lost restoration and a leaked outer stamp.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn dispatch_clock_ownership_model() -> Model {
    aterm_spec::ty_model! {
        DispatchClockOwnership {
            const Buggy = 0;
            var depth = 0;
            var current = 0;
            var saved = 0;
            var outer_remaining = 0;
            var inner_remaining = 0;
            var worker_read = 3;
            var worker_claim = 0;
            var ui_claim = 0;

            action EnterOuter when (depth == 0) {
                depth = 1;
                current = 1;
                outer_remaining = 1;
            }
            action EnterInner when (depth == 1) {
                saved = current;
                depth = 2;
                current = 2;
                inner_remaining = 2;
            }
            action ClaimUi when (depth > 0 && current > 0) {
                ui_claim = current;
                current = 0;
                outer_remaining = if depth == 1 { 0 } else { outer_remaining };
                inner_remaining = if depth == 2 { 0 } else { inner_remaining };
            }
            action ExitInner when (depth == 2) {
                depth = 1;
                current = if Buggy == 3 { 0 } else { saved };
                saved = 0;
                inner_remaining = 0;
            }
            action ExitOuter when (depth == 1) {
                depth = 0;
                current = if Buggy == 4 { current } else { 0 };
                outer_remaining = 0;
            }
            action PeekWorker when (depth <= 2) {
                worker_read = if Buggy == 1 && current > 0 { current } else { 3 };
            }
            action ClaimWorker when (depth <= 2) {
                worker_claim = if Buggy == 2 { current } else { 0 };
                current = if Buggy == 2 { 0 } else { current };
            }

            invariant UiScopeRetainsOwnedStamp:
                depth == 0 || (depth == 1 && current == outer_remaining)
                    || (depth == 2 && current == inner_remaining);
            invariant OutsideScopeDisarmed:
                depth > 0 || current == 0;
            invariant WorkerCannotBorrowOrClaim:
                worker_read == 3 && worker_claim == 0;
        }
    }
}
