// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Explicit canonical identity adoption checks the current default subscription
/// before publishing metadata; it never publishes credential authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_adopt_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountAdoptIdentity {
            const Buggy = 0;
            var case = 1; var phase = 0;
            var published = 0; var probed = 0; var granted = 0;
            action AnotherCase when (phase == 0 && case <= 20) { case = case + 1; }
            action Adopt when (phase == 0) {
                phase = 1;
                published = if case == 1 {if Buggy == 1 {0} else {1}}
                    else if case == 2 {if Buggy == 2 {0} else {1}}
                    else if case == 16 {if Buggy == 16 {0} else {1}}
                    else if case == 17 {if Buggy == 17 {0} else {1}}
                    else if case == 20 {if Buggy == 20 {0} else {1}}
                    else if (case == 3 && Buggy == 3) || (case == 4 && Buggy == 4)
                        || (case == 5 && Buggy == 5) || (case == 6 && Buggy == 6)
                        || (case == 7 && Buggy == 7) || (case == 8 && Buggy == 8)
                        || (case == 9 && Buggy == 9) || (case == 10 && Buggy == 10)
                        || (case == 11 && Buggy == 11) || (case == 12 && Buggy == 12)
                        || (case == 13 && Buggy == 13) || (case == 14 && Buggy == 14)
                        || (case == 15 && Buggy == 15) || (case == 18 && Buggy == 18)
                        || (case == 19 && Buggy == 19) || (case == 21 && Buggy == 21) {1} else {0};
                probed = if case == 11 || case == 12 || case == 16 {
                    if Buggy == 22 {1} else {0}
                } else if case == 20 && Buggy == 24 {0} else {1};
                granted = if Buggy == 23 {1} else {0};
            }
            invariant IdentityAndPolicyRequired: published == 0 || case == 1 || case == 2
                || case == 16 || case == 17 || case == 20;
            invariant CurrentAdoptionAllowed: phase == 0 || published == 1
                || (case > 2 && 16 > case) || case == 18 || case == 19 || case == 21;
            invariant NoUnneededProbe: (case > 0 && 11 > case) || (case > 12 && 16 > case)
                || case > 16 || probed == 0;
            invariant ExplicitIdentityProbed: phase == 0 || case == 11 || case == 12
                || case == 16 || probed == 1;
            invariant AdoptionGrantsNoCredentialAuthority: granted == 0;
        }
    }
}
