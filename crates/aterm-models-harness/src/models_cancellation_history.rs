// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// Cancelling a compatible, non-consumed root retains the attempt marker from
/// both disk and caller memory. This marker is not proof of native execution.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cancellation_history_model() -> Model {
    aterm_spec::ty_model! {
        CancellationAttemptHistory {
            const Buggy = 0;
            var stored = 0; var caller = 0; var fault = 0; var done = 0;
            var marker = 0; var acknowledged = 0;
            action StoredAttempt when (done == 0 && stored == 0) {
                stored = 1; marker = 1;
            }
            action CallerAttempt when (done == 0 && caller == 0) { caller = 1; }
            // Healthy storage, then each of cancellation's four directory barriers.
            action NextFault when (done == 0 && fault <= 3) { fault = fault + 1; }
            action Cancel when (done == 0) {
                done = 1;
                marker = if fault == 0 || fault == 4 {
                    if Buggy == 1 {stored} else if Buggy == 2 {caller}
                    else if stored == 1 || caller == 1 {1} else {0}
                } else {stored};
                acknowledged = if Buggy == 4 {0}
                    else if fault == 0 || Buggy == 3 {1} else {0};
            }
            invariant AcknowledgedHistory: acknowledged == 0
                || (stored <= marker && caller <= marker);
            invariant VisibleHistory: done == 0 || (fault > 0 && fault <= 3)
                || (stored <= marker && caller <= marker);
            invariant DurableReturn: acknowledged == 0 || fault == 0;
            invariant Available: done == 0 || fault > 0 || acknowledged == 1;
            bound Finite: 0 <= stored && stored <= 1 && 0 <= caller && caller <= 1
                && 0 <= fault && fault <= 4 && 0 <= done && done <= 1
                && 0 <= marker && marker <= 1
                && 0 <= acknowledged && acknowledged <= 1;
        }
    }
}
