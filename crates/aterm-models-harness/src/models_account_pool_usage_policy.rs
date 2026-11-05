// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A usage poll cannot publish or launch against policy or credentials changed during its read.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_usage_policy_model() -> Model {
    // Cases 1..=16 vary roster/binding policy. 17/18 return usage/sign-out
    // under unchanged file credentials; 19/20 replace those credentials while
    // the same response is in flight. Auth: 0 unseen, 1 OK, 2 signed out.
    aterm_spec::ty_model! {
        AccountPoolUsagePolicy {
            const Buggy = 0;
            var phase = 0; var case = 1; var published = 0; var launched = 0;
            var auth = 0; var unchanged = 1;
            action AnotherCase when (phase == 0 && case <= 19) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action PollAndChoose when (phase == 1) {
                phase = 2;
                published = if case == 1 || case == 17 { if Buggy == 10 {0} else {1} }
                    else if (case == 3 && Buggy == 1) || (case == 5 && Buggy == 3)
                        || (case == 7 && Buggy == 5) || (case == 9 && Buggy == 7)
                        || (case == 10 && Buggy == 8)
                        || (case == 13 && Buggy == 13) || (case == 15 && Buggy == 15)
                        || (case == 19 && Buggy == 17) {1} else {0};
                launched = if case <= 2 || case == 17 { if Buggy == 9 {0} else {1} }
                    else if (case == 3 && Buggy == 1) || (case == 4 && Buggy == 2)
                        || (case == 5 && Buggy == 3) || (case == 6 && Buggy == 4)
                        || (case == 7 && Buggy == 5) || (case == 8 && Buggy == 6)
                        || (case == 9 && Buggy == 7) || (case == 10 && Buggy == 8)
                        || (case == 11 && Buggy == 11) || (case == 12 && Buggy == 12)
                        || (case == 13 && Buggy == 13) || (case == 14 && Buggy == 14)
                        || (case == 15 && Buggy == 15) || (case == 16 && Buggy == 16)
                        || (case == 19 && Buggy == 17) || (case == 20 && Buggy == 18) {1} else {0};
                auth = if case == 17 {if Buggy == 19 {0} else {1}}
                    else if case == 18 {if Buggy == 20 {0} else {2}}
                    else if case == 19 && Buggy == 17 {1}
                    else if case == 20 && Buggy == 18 {2} else {0};
                unchanged = if case == 1 || case == 17 || case == 18 || (case > 18 && Buggy == 21) {0} else {1};
            }
            invariant CurrentPublication: phase <= 1 || case <= 2 || case == 17 || published == 0;
            invariant CurrentLaunch: phase <= 1 || case <= 2 || case == 17 || launched == 0;
            invariant StableLaunch: phase <= 1 || (case > 2 && case <= 16) || case > 17 || launched == 1;
            invariant StableObservation: phase <= 1 || (case > 1 && case <= 16) || case > 17 || published == 1;
            invariant CurrentAuthentication: case <= 18 || auth == 0;
            invariant PositiveAuthentication: phase <= 1 || case <= 16 || case > 18 || (case == 17 && auth == 1) || (case == 18 && auth == 2);
            invariant RefusalPreservesRecords: case == 1 || case == 17 || case == 18 || unchanged == 1;
        }
    }
}
