// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A vendor sign-in may record its result only under its captured target policy.
/// Native conformance drives login and renewal, successful and failed outcomes,
/// and independent edits to another row.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_signin_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountSignInPolicy {
            const Buggy = 0;
            var provider = 1; var mode = 1; var case = 1; var outcome = 0;
            var phase = 0; var published = 0; var kept = 1;
            action AnotherProvider when (phase == 0 && provider <= 2) { provider = provider + 1; }
            action AnotherMode when (phase == 0 && mode == 1 && outcome == 0) { mode = 2; }
            action AnotherOutcome when (phase == 0 && ((mode == 1 && outcome == 0)
                || (mode == 2 && outcome <= 1))) { outcome = if mode == 1 {3} else {outcome + 1}; }
            action AnotherCase when (phase == 0 && case <= 9) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Publish when (phase == 1) {
                phase = 2;
                published = if outcome == 3 {
                    if Buggy == 12 && case == 1 {1} else {0}
                } else if case == 1 || case == 10 {
                    if (case == 1 && Buggy == 1) || (case == 10 && Buggy == 10) {0} else {1}
                } else if (Buggy == 13 && outcome == 1 && case == 2)
                    || (Buggy == 14 && outcome == 2 && case == 2) {1
                } else if (case == 2 && Buggy == 2) || (case == 3 && Buggy == 3)
                    || (case == 4 && Buggy == 4) || (case == 5 && Buggy == 5)
                    || (case == 6 && Buggy == 6) || (case == 7 && Buggy == 7)
                    || (case == 8 && Buggy == 8) || (case == 9 && Buggy == 9) {1} else {0};
                kept = if case == 1 || case == 10 {
                    if outcome == 2 || outcome == 3 || (outcome == 1 && provider > 1) {0} else {1}
                } else if Buggy == 11 && case == 2 {0} else {1};
            }
            invariant CurrentTargetOnly: phase <= 1 || published == 0 || case == 1 || case == 10;
            invariant IndependentEditAllowed: phase <= 1 || outcome == 3
                || (case > 1 && case <= 9) || published == 1;
            invariant RefusedLoginRetained: case == 1 || case == 10 || kept == 1;
            invariant FailedLoginDoesNotPublish: outcome <= 2 || published == 0;
        }
    }
}
