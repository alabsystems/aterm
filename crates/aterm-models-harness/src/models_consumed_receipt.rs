// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// Publication after acceptance retains the accepted turn and its immutable
/// binding. This is a storage invariant, not native execution or closure proof.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn consumed_receipt_model() -> Model {
    aterm_spec::ty_model! {
        ConsumedReceiptPublication {
            const Buggy = 0;
            var kind = 0; var revoked = 0; var fault = 0; var done = 0;
            var admitted = 0; var intact = 1; var unchanged = 1; var acknowledged = 0;
            var barrier = 0;
            // Exact retry, phase regression, another turn, another binding,
            // cleared attempt, cleared revocation, descendant, revocation,
            // an unreadable root, changed attempt/candidate and cleared PID/birth.
            action NextCase when (done == 0 && kind <= 11) { kind = kind + 1; }
            action PriorRevocation when (done == 0 && revoked == 0) { revoked = 1; }
            // Healthy storage, a pre-rename failure, a post-rename failure.
            action NextFault when (done == 0 && fault <= 1) { fault = fault + 1; }
            action Publish when (done == 0) {
                done = 1;
                admitted = if kind == 8 {if Buggy == 2 {1} else {0}}
                    else if kind == 1 || kind == 2 || kind == 3 || kind == 4
                        || (kind == 5 && revoked == 1) || kind > 8 {if Buggy == 1 {1} else {0}}
                    else if Buggy == 6 && (kind == 6 || kind == 7) {0}
                    else {1};
                intact = if (fault == 0 || fault == 2)
                    && ((Buggy == 1 && (kind == 1 || kind == 2 || kind == 3 || kind == 4
                        || (kind == 5 && revoked == 1) || kind > 8)) || (Buggy == 2 && kind == 8))
                    {0} else {1};
                unchanged = if Buggy == 6 && (kind == 6 || kind == 7) {1}
                    else if (Buggy == 5 && (kind == 1 || kind == 2 || kind == 3
                        || kind == 4 || (kind == 5 && revoked == 1) || kind > 7))
                    || ((fault == 0 || fault == 2)
                        && (kind == 6 || (kind == 7 && revoked == 0)
                            || (Buggy == 1 && (kind == 1 || kind == 2 || kind == 3 || kind == 4
                                || (kind == 5 && revoked == 1) || kind > 8))
                            || (Buggy == 2 && kind == 8))) {0} else {1};
                acknowledged = if Buggy == 4 || (Buggy == 6 && (kind == 6 || kind == 7)) {0}
                    else if (fault == 0 || (Buggy == 3 && kind == 0))
                        && (kind == 0 || (kind == 5 && revoked == 0) || kind == 6 || kind == 7
                            || (Buggy == 1 && (kind == 1 || kind == 2 || kind == 3 || kind == 4
                                || (kind == 5 && revoked == 1) || kind > 8))
                            || (Buggy == 2 && kind == 8)) {1}
                    else {0};
                barrier = if (Buggy == 3 && kind == 0)
                    || (Buggy == 6 && (kind == 6 || kind == 7)) {0}
                    else if kind == 8 {if Buggy == 2 {1} else {0}}
                    else if kind == 1 || kind == 2 || kind == 3 || kind == 4
                        || (kind == 5 && revoked == 1) || kind > 8 {if Buggy == 1 {1} else {0}}
                    else {1};
            }
            invariant AcceptedHistory: intact == 1;
            invariant RefusalPreservesBytes: admitted == 1 || unchanged == 1;
            invariant DurableReturn: acknowledged == 0 || fault == 0;
            invariant BarrierBeforeAcknowledgement: acknowledged == 0 || barrier == 1;
            invariant Available: done == 0 || fault > 0 || acknowledged == 1
                || kind == 1 || kind == 2 || kind == 3 || kind == 4
                || (kind == 5 && revoked == 1) || kind > 7;
            bound Finite: 0 <= kind && kind <= 12 && 0 <= revoked && revoked <= 1
                && 0 <= fault && fault <= 2 && 0 <= done && done <= 1
                && 0 <= admitted && admitted <= 1 && 0 <= intact && intact <= 1
                && 0 <= unchanged && unchanged <= 1
                && 0 <= acknowledged && acknowledged <= 1 && 0 <= barrier && barrier <= 1;
        }
    }
}
