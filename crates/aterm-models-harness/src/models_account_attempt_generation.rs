// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Login and renewal results cannot change a replacement binding.
/// Native conformance runs both login and renewal for each supported provider.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_attempt_generation_model() -> Model {
    aterm_spec::ty_model! {
        AccountAttemptGeneration {
            const Buggy = 0;
            var provider = 1; var mode = 1; var case = 1;
            var phase = 0; var published = 0; var preserved = 1;
            action AnotherProvider when (phase == 0 && provider <= 3) { provider = provider + 1; }
            action AnotherMode when (phase == 0 && mode == 1) { mode = 2; }
            action AnotherCase when (phase == 0 && case <= 8) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Commit when (phase == 1) {
                phase = 2;
                published = if case == 1 || case == 5 || case == 9 {
                    if (case == 1 && Buggy == 1) || (case == 5 && Buggy == 5)
                        || (case == 9 && Buggy == 9) {0} else {1}
                } else if (case == 2 && Buggy == 2) || (case == 3 && Buggy == 3)
                    || (case == 4 && Buggy == 4) || (case == 6 && Buggy == 6)
                    || (case == 7 && Buggy == 7) || (case == 8 && Buggy == 8) {1} else {0};
                preserved = if case == 1 || case == 5 || case == 9
                    || (Buggy == 10 && case == 6) {0} else {1};
            }
            invariant StaleAttemptRefused: phase <= 1 || published == 0
                || case == 1 || case == 5 || case == 9;
            invariant CurrentAttemptPublishes: phase <= 1
                || (case > 1 && case <= 4) || (case > 5 && case <= 8) || published == 1;
            invariant RefusalPreservesRecords: published == 1 || preserved == 1;
        }
    }
}
