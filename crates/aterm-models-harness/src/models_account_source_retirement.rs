// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Fixed-role retirement across state-only, roster and fleet receipts. This
/// models durable local exclusion; it makes no authenticated reactivation claim.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_retirement_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourceRetirement {
            const Buggy = 0;
            var mode = 0; var cut = 0; var conflict = 0; var done = 0;
            var retired = 0; var fenced = 1; var preserved = 1; var loss_refused = 1; var stable = 1;
            action Mode when (done == 0 && 5 > mode) { mode = mode + 1; }
            action Cut when (done == 0 && 7 > cut) { cut = cut + 1; }
            action Conflict when (done == 0 && conflict == 0) { conflict = 1; }
            action Commit when (done == 0) {
                done = 1;
                retired = if conflict == 1 || Buggy == 1 {0} else {1};
                fenced = if Buggy == 4 && cut == 0 {0} else {1};
                preserved = if Buggy == 2 && mode == 2 && cut == 0 && conflict == 1 {0} else {1};
                loss_refused = if Buggy == 3 && conflict == 0 {0} else {1};
                stable = if Buggy == 5 && conflict == 0 {0} else {1};
            }
            invariant CommittedRetires: done == 0 || conflict == 1 || retired == 1;
            invariant PendingFencesPolicy: fenced == 1;
            invariant ConflictPreservesEveryImage: preserved == 1;
            invariant LostPolicyIsNotInitial: loss_refused == 1;
            invariant RepeatedRetirementIsStable: stable == 1;
            bound ModeDomain: 0 <= mode && mode <= 5;
            bound CutDomain: 0 <= cut && cut <= 7;
        }
    }
}
