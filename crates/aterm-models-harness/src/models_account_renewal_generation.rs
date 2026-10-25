// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Renewals and binding identity changes advance a local generation independently of time.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_renewal_generation_model() -> Model {
    aterm_spec::ty_model! {
        AccountRenewalGeneration {
            const Buggy = 0;
            var phase = 0; var case = 1; var accepted = 0; var advanced = 0; var preserved = 1;
            action AnotherCase when (phase == 0 && case <= 14) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Commit when (phase == 1) {
                phase = 2;
                accepted = if case == 5 || case == 6 || case == 9 || case == 14 || case == 15 {
                    if (case == 5 && Buggy == 5) || (case == 6 && Buggy == 6)
                        || (case == 9 && Buggy == 7) || (case == 14 && Buggy == 15)
                        || (case == 15 && Buggy == 17) {1} else {0}
                } else if case == 3 && Buggy == 19 {0} else {1};
                advanced = if case <= 3 || case == 7 || (case > 9 && case <= 13) {
                    if (case == 1 && Buggy == 1) || (case == 2 && Buggy == 2)
                        || (case == 3 && Buggy == 3) || (case == 7 && Buggy == 4)
                        || (case == 10 && Buggy == 11) || (case == 11 && Buggy == 12)
                        || (case == 12 && Buggy == 13) || (case == 13 && Buggy == 14) {0} else {1}
                } else if (case == 4 && Buggy == 8) || (case == 8 && Buggy == 9) {1} else {0};
                preserved = if (case == 5 && Buggy == 10) || (case == 14 && Buggy == 16)
                    || (case == 15 && Buggy == 18) {0} else {1};
            }
            invariant RenewalAdvances: phase <= 1 || case == 4 || case == 5 || case == 6
                || case == 8 || case == 9 || case > 13 || advanced == 1;
            invariant OtherKeeps: phase <= 1 || case <= 3 || case == 7
                || (case > 9 && case <= 13) || advanced == 0;
            invariant AllowedPublishes: phase <= 1 || case == 5 || case == 6 || case == 9 || case > 13 || accepted == 1;
            invariant Refused: phase <= 1 || case <= 4 || case == 7 || case == 8
                || (case > 9 && case <= 13) || accepted == 0;
            invariant RefusalRetains: accepted == 1 || preserved == 1;
        }
    }
}
