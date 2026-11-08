// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// A cancelled native reservation may undo only its own still-current canary.
/// Session and account names alone cannot distinguish a later reservation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_canary_model() -> Model {
    aterm_spec::ty_model! {
        NativeCanaryOwnership {
            const Buggy = 0;
            // No canary, our receipt, another receipt, unknown legacy owner.
            var owner = 0; var taken = 0; var mapped = 1; var done = 0;
            var remaining = 0; var refund = 0; var removed = 0; var other_removed = 0;
            action NextOwner when (done == 0 && owner <= 2) {
                owner = owner + 1; remaining = owner + 1;
            }
            action Taken when (done == 0 && taken == 0) { taken = 1; }
            action Forget when (done == 0 && mapped == 1) { mapped = 0; }
            action Release when (done == 0) {
                done = 1;
                remaining = if Buggy == 2 {owner}
                    else if (mapped == 1 || Buggy == 7) && (owner == 1
                        || (Buggy == 1 && owner > 0)) {0}
                    else if Buggy == 3 && owner == 2 {0} else {owner};
                refund = if Buggy == 2 {0}
                    else if (mapped == 1 || Buggy == 7) && (owner == 1
                        || (Buggy == 1 && owner > 0)) && (taken == 0 || Buggy == 4) {1}
                    else if Buggy == 3 && owner == 2 && taken == 0 {1}
                    else if Buggy == 5 && owner > 1 && mapped == 1 {1} else {0};
                removed = if Buggy == 2 || Buggy == 6 || (Buggy == 3 && owner == 2) {0}
                    else {mapped};
                other_removed = if Buggy == 3 && owner == 2 {1} else {0};
            }
            invariant ForeignCanary: done == 0 || owner == 1 || remaining == owner;
            invariant ForeignBudget: done == 0 || owner == 1 || refund == 0;
            invariant OwnCanary: done == 0 || owner <= 0 || owner > 1 || mapped == 0 || remaining == 0;
            invariant OwnRefund: done == 0 || owner <= 0 || owner > 1
                || mapped == 0 || taken == 1 || refund == 1;
            invariant TakenBudget: done == 0 || taken == 0 || refund == 0;
            invariant ExactRetirement: done == 0 || removed == mapped;
            invariant OtherReceipt: other_removed == 0;
            invariant MissingReceipt: done == 0 || mapped == 1
                || (remaining == owner && refund == 0);
            bound Finite: 0 <= owner && owner <= 3 && 0 <= taken && taken <= 1
                && 0 <= mapped && mapped <= 1 && 0 <= done && done <= 1
                && 0 <= remaining && remaining <= 3 && 0 <= refund && refund <= 1
                && 0 <= removed && removed <= 1 && 0 <= other_removed && other_removed <= 1;
        }
    }
}
