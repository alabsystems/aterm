// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Inventory counts intersect policy candidates with a supplied captured identity verdict.
/// The verdict is an input, not proof of filesystem identity or snapshot freshness.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_progress_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountProgressIdentity {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var policy = 0; var checked = 0; var check_set = 1;
            var eligible = 3; var admitted = 0; var denied = 0;
            var findings = 0; var preserved = 1;
            action Case when (phase == 0 && 14 > case) { case = case + 1; }
            action Project when (phase == 0) {
                phase = 1;
                policy = if case == 7 || case == 8 || case == 9 {2}
                    else if case <= 3 || case == 11 {1} else {0};
                checked = if Buggy == 2 && case == 4 {1}
                    else if case == 7 || case == 8 || case == 9 {2}
                    else if case <= 3 || case == 11 {1} else {0};
                check_set = if Buggy == 2 && case == 4 {0} else {1};
                admitted = if case == 9 {2}
                    else if case == 1 || case == 3 || case == 7 || case == 11 {1} else {0};
                eligible = if Buggy == 1 && case == 0 {1}
                    else if case == 10 {if Buggy == 4 {0} else {3}}
                    else if case == 9 {2}
                    else if case == 1 || case == 3 || case == 7 || case == 11 {1} else {0};
                denied = if case == 8 {2}
                    else if case == 0 || case == 2 || case == 7 {1} else {0};
                findings = if Buggy == 3 && case == 0 {0}
                    else if Buggy == 3 && case == 1 {1}
                    else if case == 0 || case == 2 || case == 7 || case == 8 {1} else {0};
                preserved = if Buggy == 5 {0} else {1};
            }
            invariant CountUsesCapturedIdentity: phase == 0 || case == 10 || eligible == admitted;
            invariant OnlyPolicyEligibleAlternativesChecked: checked == policy && check_set == 1;
            invariant UnjudgedRemainsUnknown: phase == 0 || 10 > case || case > 10
                || (eligible == 3 && checked == 0);
            invariant DenialFindingIsTruthful: findings == if denied > 0 {1} else {0};
            invariant CapturedFactsPreserved: preserved == 1;
        }
    }
}
