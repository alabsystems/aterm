// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Input classes at ordinary account admission. The actual host projection
/// independently constructs each failed premise and calls its private entry.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_continuation_model() -> Model {
    aterm_spec::ty_model! {
        AccountContinuation {
            const Buggy = 0;
            var case = 0; var checked = 0; var accepted = 0;
            action Exited when (checked == 0) { case = 1; }
            action Grant when (checked == 0) { case = 2; }
            action Capability when (checked == 0) { case = 3; }
            action Actor when (checked == 0) { case = 4; }
            action Conversation when (checked == 0) { case = 5; }
            action Recipe when (checked == 0) { case = 6; }
            action Phase when (checked == 0) { case = 7; }
            action Policy when (checked == 0) { case = 8; }
            action Cohort when (checked == 0) { case = 9; }
            action AccountPool when (checked == 0) { case = 10; }
            action Check when (checked == 0) {
                checked = 1;
                accepted = if case == 0 { if Buggy == 11 {0} else {1} }
                    else { if (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                        || (Buggy == 3 && case == 3) || (Buggy == 4 && case == 4)
                        || (Buggy == 5 && case == 5) || (Buggy == 6 && case == 6)
                        || (Buggy == 7 && case == 7) || (Buggy == 8 && case == 8)
                        || (Buggy == 9 && case == 9) || (Buggy == 10 && case == 10) {1} else {0} };
            }
            invariant NoExited: accepted == 0 || case <= 0 || case > 1;
            invariant NoGrant: accepted == 0 || case <= 1 || case > 2;
            invariant ExactCapability: accepted == 0 || case <= 2 || case > 3;
            invariant ExactActor: accepted == 0 || case <= 3 || case > 4;
            invariant CurrentConversation: accepted == 0 || case <= 4 || case > 5;
            invariant ExactRecipe: accepted == 0 || case <= 5 || case > 6;
            invariant SamePhase: accepted == 0 || case <= 6 || case > 7;
            invariant OrdinaryPolicy: accepted == 0 || case <= 7 || case > 8;
            invariant Quiescent: accepted == 0 || case <= 8 || case > 9;
            invariant EligibleAccount: accepted == 0 || case <= 9;
            invariant ValidAdmitted: checked == 0 || case > 0 || accepted == 1;
            bound Finite: 0 <= case && case <= 10 && 0 <= checked && checked <= 1
                && 0 <= accepted && accepted <= 1;
        }
    }
}
