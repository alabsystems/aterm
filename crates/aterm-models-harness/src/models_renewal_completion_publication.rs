// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One local publication poll over an exact retained attempt. Failures cover
/// the native lock and both sides of receipt rename. Execution, kernel NoExec
/// derivation, service scheduling and eventual retries are outside this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn renewal_completion_publication_model() -> Model {
    aterm_spec::ty_model! {
        RenewalCompletionPublication {
            const Buggy = 0;
            var case = 0; var failure = 0; var done = 0;
            var native_ack = 0; var visible = 0; var acknowledged = 0;
            action NextCase when (done == 0 && failure == 0 && case <= 7) { case = case + 1; }
            action NextFailure when (done == 0 && failure <= 2 && (failure == 0 || case <= 2)) {
                failure = failure + 1;
            }
            action Recover when (done == 0) {
                done = 1;
                native_ack = if (case <= 3 || Buggy == 1 || Buggy == 4) && (failure == 0 || failure > 1) {1} else {0};
                visible = if case == 3 && Buggy == 3 {1}
                    else if case <= 2 && failure == 1 && Buggy == 6 {1}
                    else if (case <= 2 || Buggy == 1) && (failure == 0 || failure == 3) {1}
                    else {0};
                acknowledged = if Buggy == 5 {0}
                    else if (case <= 3 || Buggy == 1) && (failure == 0 || Buggy == 2) {1}
                    else {0};
            }
            invariant ExactReceipt: acknowledged == 0 || case <= 3;
            invariant NativeBeforeReceipt: visible == 0 || native_ack == 1;
            invariant DurableReturn: acknowledged == 0 || failure == 0;
            invariant ChildPreserved: case <= 2 || case > 3 || visible == 0;
            invariant SilentUnknown: case <= 3 || (native_ack == 0 && visible == 0);
            invariant Available: done == 0 || case > 3 || failure > 0 || acknowledged == 1;
            bound Finite: 0 <= case && case <= 8 && 0 <= failure && failure <= 3
                && 0 <= done && done <= 1 && 0 <= native_ack && native_ack <= 1
                && 0 <= visible && visible <= 1 && 0 <= acknowledged && acknowledged <= 1;
        }
    }
}
