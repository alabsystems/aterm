// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// State-only account writes preserve paired history and recover without roster authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_state_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountStateCommit {
            const Buggy = 0;
            var phase = 0; var provider = 1; var operation = 1; var fault = 1;
            var success = 0; var coherent = 1; var isolated = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) { provider = provider + 1; }
            action AnotherOperation when (phase == 0 && 3 > operation) { operation = operation + 1; }
            action AnotherFault when (phase == 0 && 7 > fault) { fault = fault + 1; }
            action Write when (phase == 0) {
                phase = 1;
                success = if Buggy == 1 {1} else {if fault == 1 {1} else {0}};
                coherent = if Buggy == 2 {0} else {1};
                isolated = if Buggy == 3 {0} else {1};
            }
            action Retry when (phase == 1) {
                phase = 2;
                success = if fault == 4 || fault == 5 || fault == 7 {
                    if Buggy == 5 {1} else {0}
                } else {if Buggy == 4 {0} else {1}};
                coherent = if Buggy == 2 {0} else {1};
                isolated = if Buggy == 3 {0} else {1};
            }
            invariant WriteAvailability: phase == 0 || phase == 2 || success == (if fault == 1 {1} else {0});
            invariant RetryAvailability: phase <= 1 || fault == 4 || fault == 5 || fault == 7 || success == 1;
            invariant ConflictRefused: phase <= 1 || fault == 1 || fault == 2 || fault == 3 || fault == 6 || success == 0;
            invariant StateAndHistoryTogether: coherent == 1;
            invariant UnrelatedAuthorityPreserved: isolated == 1;
        }
    }
}
