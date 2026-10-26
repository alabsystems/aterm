// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Renewal write failures remain fenced until the exact admitted generation is recovered.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_renewal_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountRenewalCommit {
            const Buggy = 0;
            var phase = 0; var provider = 1; var case = 1;
            var recovered = 0; var fenced = 1; var exact = 1;
            var once = 1; var parent = 1; var preserved = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) {provider = provider + 1;}
            action AnotherCase when (phase == 0 && 8 > case) {case = case + 1;}
            action Commit when (phase == 0) {
                phase = 1;
                recovered = if 4 > case || case == 8 {
                    if Buggy == 1 {0} else {1}
                } else {if Buggy == 2 {1} else {0}};
                fenced = if Buggy == 3 {0} else {1};
                exact = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                parent = if Buggy == 6 {0} else {1};
                preserved = if Buggy == 7 {0} else {1};
            }
            invariant AdmittedRenewalCompletes: phase == 0 || (case > 3 && 8 > case) || recovered == 1;
            invariant ConflictsRefuse: 4 > case || case == 8 || recovered == 0;
            invariant IncompleteRenewalFenced: fenced == 1;
            invariant ExactStateAndBinding: exact == 1;
            invariant LedgerAndGenerationOnce: once == 1;
            invariant CapturedParentExpires: parent == 1;
            invariant CredentialsAndConflictsPreserved: preserved == 1;
        }
    }
}
