// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A captured renewal cannot publish after policy, binding or retained records change.
/// The native transition exercises the locked store transaction and checkpoint writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_renewal_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountRenewalPolicy {
            const Buggy = 0;
            var phase = 0; var case = 1; var published = 0;
            action AnotherCase when (phase == 0 && case <= 16) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Publish when (phase == 1) {
                phase = 2;
                published = if case == 1 || case == 13 || case == 14 || case == 17 {
                    if (case == 1 && Buggy == 9) || (case == 13 && Buggy == 13)
                        || (case == 14 && Buggy == 14) || (case == 17 && Buggy == 17) {0} else {1} }
                    else if (case == 2 && Buggy == 1) || (case == 3 && Buggy == 2)
                        || (case == 4 && Buggy == 3) || (case == 5 && Buggy == 4)
                        || (case == 6 && Buggy == 5) || (case == 7 && Buggy == 6)
                        || (case == 8 && Buggy == 7) || (case == 9 && Buggy == 8)
                        || (case == 10 && Buggy == 10) || (case == 11 && Buggy == 11)
                        || (case == 12 && Buggy == 12) || (case == 15 && Buggy == 15)
                        || (case == 16 && Buggy == 16) {1} else {0};
            }
            invariant CurrentPolicyOnly: phase <= 1 || published == 0
                || case == 1 || case == 13 || case == 14 || case == 17;
            invariant CurrentPolicyPublishes: phase <= 1
                || (case > 1 && case <= 12) || case == 15 || case == 16 || published == 1;
        }
    }
}
