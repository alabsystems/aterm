// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A usage poll cannot publish or launch against a policy changed during its read.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_usage_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolUsagePolicy {
            const Buggy = 0;
            var phase = 0; var case = 1; var published = 0; var launched = 0;
            action AnotherCase when (phase == 0 && case <= 15) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action PollAndChoose when (phase == 1) {
                phase = 2;
                published = if case == 1 { if Buggy == 10 {0} else {1} }
                    else if (case == 3 && Buggy == 1) || (case == 5 && Buggy == 3)
                        || (case == 7 && Buggy == 5) || (case == 9 && Buggy == 7)
                        || (case == 10 && Buggy == 8)
                        || (case == 13 && Buggy == 13) || (case == 15 && Buggy == 15) {1} else {0};
                launched = if case <= 2 { if Buggy == 9 {0} else {1} }
                    else if (case == 3 && Buggy == 1) || (case == 4 && Buggy == 2)
                        || (case == 5 && Buggy == 3) || (case == 6 && Buggy == 4)
                        || (case == 7 && Buggy == 5) || (case == 8 && Buggy == 6)
                        || (case == 9 && Buggy == 7) || (case == 10 && Buggy == 8)
                        || (case == 11 && Buggy == 11) || (case == 12 && Buggy == 12)
                        || (case == 13 && Buggy == 13) || (case == 14 && Buggy == 14)
                        || (case == 15 && Buggy == 15) || (case == 16 && Buggy == 16) {1} else {0};
            }
            invariant CurrentPublication: phase <= 1 || case <= 2 || published == 0;
            invariant CurrentLaunch: phase <= 1 || case <= 2 || launched == 0;
            invariant StableLaunch: phase <= 1 || case > 2 || launched == 1;
            invariant StableObservation: phase <= 1 || case > 1 || published == 1;
        }
    }
}
