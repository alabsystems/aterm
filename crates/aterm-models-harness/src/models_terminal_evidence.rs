// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// Every combination of the six durable execution witnesses, at both cleanup
/// entrypoints. Metadata absence is necessary for release, not native authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn terminal_evidence_model() -> Model {
    aterm_spec::ty_model! {
        NativeTerminalEvidence {
            const Buggy = 0;
            // Bits: issued marker, accepted turn, attempt, candidate, PID, birth.
            var case = 0; var retirement = 0; var done = 0;
            var admitted = 0; var removed = 0; var refunded = 0; var rewritten = 0;
            action NextCase when (done == 0 && case <= 62) { case = case + 1; }
            action Retirement when (done == 0 && retirement == 0) { retirement = 1; }
            action Apply when (done == 0) {
                done = 1;
                admitted = if Buggy == 7 {0} else if case == 0
                    || (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                    || (Buggy == 3 && case == 4) || (Buggy == 4 && case == 8)
                    || (Buggy == 5 && case == 16) || (Buggy == 6 && case == 32) {1} else {0};
                // Action updates read the same prior state, not preceding assignments.
                removed = if Buggy == 7 {0} else if case == 0
                    || (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                    || (Buggy == 3 && case == 4) || (Buggy == 4 && case == 8)
                    || (Buggy == 5 && case == 16) || (Buggy == 6 && case == 32) {1} else {0};
                refunded = if Buggy == 7 {0} else if (retirement == 0 && (case == 0
                    || (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                    || (Buggy == 3 && case == 4) || (Buggy == 4 && case == 8)
                    || (Buggy == 5 && case == 16) || (Buggy == 6 && case == 32)))
                    || (Buggy == 8 && retirement == 1) {1} else {0};
                rewritten = if Buggy == 7 {0} else if (retirement == 0 && (case == 0
                    || (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                    || (Buggy == 3 && case == 4) || (Buggy == 4 && case == 8)
                    || (Buggy == 5 && case == 16) || (Buggy == 6 && case == 32)))
                    || (Buggy == 9 && case > 0) {1} else {0};
            }
            invariant NoConflictingCleanup: case == 0 || removed == 0;
            invariant NoConflictingAdmission: case == 0 || admitted == 0;
            invariant NoConflictingRefund: case == 0 || refunded == 0;
            invariant AvailableCleanup: done == 0 || case > 0 || removed == 1;
            invariant CancellationRefund: done == 0 || case > 0 || retirement == 1 || refunded == 1;
            invariant RetirementKeepsBudget: retirement == 0 || refunded == 0;
            invariant ConflictingReceiptUnchanged: case == 0 || rewritten == 0;
            bound Finite: 0 <= case && case <= 63 && 0 <= retirement && retirement <= 1
                && 0 <= done && done <= 1 && 0 <= admitted && admitted <= 1
                && 0 <= removed && removed <= 1 && 0 <= refunded && refunded <= 1
                && 0 <= rewritten && rewritten <= 1;
        }
    }
}
