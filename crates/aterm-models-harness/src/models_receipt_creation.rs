// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Initial root publication under its account writer. Existing artifacts,
/// cancellation and a refused final directory barrier are independent inputs.
/// This bounds one creation/retry; it grants no native admission or execution.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn receipt_creation_model() -> Model {
    aterm_spec::ty_model! {
        ReceiptCreation {
            const Buggy = 0;
            var existing = 0; var cancelled = 0; var failed = 0;
            var done = 0; var accepted = 0; var preserved = 1;
            action Existing when (done == 0 && existing <= 6) { existing = existing + 1; }
            action Cancellation when (done == 0 && cancelled <= 1) { cancelled = cancelled + 1; }
            action Failed when (done == 0 && failed == 0) { failed = 1; }
            action Create when (done == 0) {
                done = 1;
                preserved = if Buggy == 1 && existing == 2 {0} else {1};
                accepted = if Buggy == 4 {0}
                    else if (existing <= 1 || (Buggy == 1 && existing == 2))
                        && (cancelled == 0 || Buggy == 2)
                        && (failed == 0 || Buggy == 3) {1} else {0};
            }
            invariant PreserveArtifact: preserved == 1;
            invariant ExactCreation: accepted == 0 || existing <= 1;
            invariant RespectCancellation: accepted == 0 || cancelled == 0;
            invariant Acknowledged: accepted == 0 || failed == 0;
            invariant Available: done == 0 || existing > 1 || cancelled > 0
                || failed == 1 || accepted == 1;
            bound Finite: 0 <= existing && existing <= 7
                && 0 <= cancelled && cancelled <= 2 && 0 <= failed && failed <= 1
                && 0 <= done && done <= 1 && 0 <= accepted && accepted <= 1
                && 0 <= preserved && preserved <= 1;
        }
    }
}
