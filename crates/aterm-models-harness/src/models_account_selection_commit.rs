// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Explicit selections use current policy and publish their budget and history together.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_selection_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountSelectionCommit {
            const Buggy = 0;
            var phase = 0; var provider = 1; var case = 1;
            var outcome = 0; var guarded = 1; var coherent = 1; var once = 1; var preserved = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) {provider = provider + 1;}
            action AnotherCase when (phase == 0 && 12 > case) {case = case + 1;}
            action Select when (phase == 0 && 8 > case) {
                phase = 1;
                outcome = if case == 1 {if Buggy == 2 {0} else {1}}
                    else if case == 2 {if Buggy == 3 {0} else {2}}
                    else {if Buggy == 1 {1} else {0}};
                guarded = if Buggy == 5 {0} else {1};
                coherent = if Buggy == 6 {0} else {1};
                once = if Buggy == 7 {0} else {1};
                preserved = if Buggy == 8 {0} else {1};
            }
            action Complete when (phase == 0 && case > 7) {
                phase = 1;
                outcome = if case > 10 {if Buggy == 9 {3} else {4}}
                    else if case == 10 {if Buggy == 1 {3} else {0}} else {if Buggy == 4 {0} else {3}};
                guarded = if Buggy == 5 {0} else {1};
                coherent = if Buggy == 6 {0} else {1};
                once = if Buggy == 7 {0} else {1};
                preserved = if Buggy == 8 {0} else {1};
            }
            invariant PinAvailable: phase == 0 || case > 1 || outcome == 1;
            invariant RequestAvailable: phase == 0 || case == 1 || case > 2 || outcome == 2;
            invariant CompletionAvailable: phase == 0 || 8 > case || case > 9 || outcome == 3;
            invariant InvalidCompletionCancelled: phase == 0 || 11 > case || outcome == 4;
            invariant NoFalseAcknowledgment: outcome == 0 || case == 1 || case == 2 || case == 8 || case == 9 || (case > 10 && outcome == 4);
            invariant CurrentPolicyAndFence: guarded == 1;
            invariant CoherentBudgetAndIntent: coherent == 1;
            invariant NoDuplicateUseOrGeneration: once == 1;
            invariant RetainsCredentialsAndPolicy: preserved == 1;
        }
    }
}
