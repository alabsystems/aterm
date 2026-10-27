// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Completing an account move requires current, stored-login evidence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_application_model() -> Model {
    aterm_spec::ty_model! {
        AccountApplication {
            const Buggy = 0;
            var phase = 0; var case = 1; var outcome = 0;
            action AnotherCase when (phase == 0 && case <= 9) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                outcome = if case <= 2 {
                    if (case == 1 && Buggy == 1) || (case == 2 && Buggy == 2) {2} else {1}
                } else {
                    if case == 3 || case == 7 || case == 8 || case == 10 {
                        if (case == 3 && Buggy == 3) || (case == 7 && Buggy == 7)
                            || (case == 8 && Buggy == 8) || (case == 10 && Buggy == 10) {2} else {3}
                    } else {
                        if (case == 4 && Buggy == 4) || (case == 5 && Buggy == 5)
                            || (case == 6 && Buggy == 6) || (case == 9 && Buggy == 9) {1}
                        else {if case == 4 && Buggy == 11 {3} else {2}}
                    }
                };
            }
            invariant CurrentStoredOnly: phase <= 1 || outcome == 2 || outcome == 3 || case <= 2;
            invariant AvailableProgress: phase <= 1 || case > 2 || outcome == 1;
            invariant UnprovenIsUnknown: phase <= 1 || case <= 2 || case == 4 || case == 5 || case == 6 || case == 9 || outcome == 3;
            invariant KnownRefusal: phase <= 1 || case <= 3 || case == 7 || case == 8 || case == 10 || outcome == 2;
        }
    }
}
