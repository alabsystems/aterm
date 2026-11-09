// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Canonical identity completion preserves prior ends and use through a repeated sync.
/// Cases are a healthy matching default, two sign-out times, two independent end
/// times, a mismatched default identity and an absent default identity.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_identity_completion_model() -> Model {
    aterm_spec::ty_model! {
        AccountIdentityCompletion {
            const Buggy = 0;
            var phase = 0; var case = 0;
            var ready = 0; var usage_kept = 1; var end_kept = 1; var stable = 1;
            action AnotherCase when (phase == 0 && 6 > case) { case = case + 1; }
            action Complete when (phase == 0) {
                phase = 1;
                ready = if Buggy == 1 && case > 0 && 5 > case {1}
                    else if Buggy == 2 && case == 0 {0}
                    else if Buggy == 3 && case > 4 {1}
                    else if case == 0 {1} else {0};
                usage_kept = if Buggy == 4 {0} else {1};
                end_kept = if Buggy == 5 && case > 0 && 5 > case {0} else {1};
            }
            action Repeat when (phase == 1) {
                phase = 2;
                stable = if Buggy == 6 {0} else {1};
            }
            invariant EndedLoginStaysUnusable: phase == 0 || case == 0 || case > 4 || ready == 0;
            invariant HealthyDefaultAdmitted: phase == 0 || case > 0 || ready == 1;
            invariant IdentityObservationRequired: phase == 0 || 5 > case || ready == 0;
            invariant PriorUsePreserved: usage_kept == 1;
            invariant PriorEndPreserved: end_kept == 1;
            invariant RepeatedCompletionStable: stable == 1;
            bound CaseDomain: 0 <= case && case <= 6;
            bound PhaseDomain: 0 <= phase && phase <= 2;
        }
    }
}
