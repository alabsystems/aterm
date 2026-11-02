// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Each review wave refreshes its account without manufacturing account history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_review_preparation_model() -> Model {
    aterm_spec::ty_model! {
        AccountReviewPreparation {
            const Buggy = 0;
            var scope = 0; var change = 0; var phase = 0;
            var ready = 0; var managed = 0; var isolated = 0; var preserved = 1;
            action Scope when (phase == 0 && 2 > scope) { scope = scope + 1; }
            action Change when (phase == 0 && 4 > change) { change = change + 1; }
            action Prepare when (phase == 0) {
                phase = 1;
                ready = if Buggy == 1 {0} else if Buggy == 2 || (Buggy == 6 && change == 4) || 2 > change {1} else {0};
                managed = if Buggy == 3 {1} else if 2 > change && (scope > 0 || change == 1) {1} else {0};
                isolated = if Buggy == 4 {0} else if 2 > change && (scope == 2 || change == 1) {1} else {0};
                preserved = if Buggy == 5 {0} else {1};
            }
            invariant ReadyRuns: phase == 0 || change > 1 || ready == 1;
            invariant RefusalStops: phase == 0 || 2 > change || ready == 0;
            invariant ImplicitStaysUnmanaged: phase == 0 || scope > 0 || change > 0 || managed == 0;
            invariant CurrentSeam: phase == 0 || change > 1 || (2 > scope && change == 0) || isolated == 1;
            invariant ReadPreservesHistory: preserved == 1;
            invariant PreparationHeld: phase == 0 || 4 > change || ready == 0;
        }
    }
}
