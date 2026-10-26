// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A prepared relaunch line still requires current endpoint policy, generations
/// and eligibility, with one current policy snapshot before preparation. Following
/// an active account has the same admission check.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_move_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolMovePolicy {
            const Buggy = 0;
            var provider = 1; var follows = 0; var case = 1;
            var phase = 0; var line = 0; var committed = 0;
            action AnotherProvider when (phase == 0 && provider <= 2 && case == 1) { provider = provider + 1; }
            action FollowActive when (phase == 0 && follows == 0) { follows = 1; }
            action AnotherCase when (phase == 0 && provider == 1 && case <= 23) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Finish when (phase == 1) {
                phase = 2;
                line = if provider > 1 {if provider == 2 && Buggy == 22 {1} else {0}}
                    else if case == 1 {if Buggy == 1 {0} else {1}}
                    else if case == 13 {if Buggy == 13 {0} else {1}}
                    else if case == 14 {if Buggy == 14 {0} else {1}}
                    else if case == 15 {if Buggy == 15 {0} else {1}}
                    else if (case == 2 && Buggy == 2) || (case == 3 && Buggy == 3)
                        || (case == 4 && Buggy == 4) || (case == 5 && Buggy == 5)
                        || (case == 6 && Buggy == 6) || (case == 7 && Buggy == 7)
                        || (case == 8 && Buggy == 8) || (case == 9 && Buggy == 9)
                        || (case == 10 && Buggy == 10) || (case == 11 && Buggy == 11)
                        || (case == 12 && Buggy == 12) || (case == 16 && Buggy == 16)
                        || (case == 17 && Buggy == 17) || (case == 18 && Buggy == 18)
                        || (case == 19 && Buggy == 23) || (case == 20 && Buggy == 24)
                        || (case == 21 && Buggy == 25) || (case == 22 && Buggy == 26)
                        || (case == 23 && Buggy == 27) || (case == 24 && Buggy == 28) {1} else {0};
                committed = if provider > 1 {0}
                    else if (case == 1 || case == 13 || case == 14 || case == 15) && follows == 0 {
                    if case == 1 && Buggy == 20 {0} else {1}
                } else if case == 2 && Buggy == 19 {1}
                    else if case == 1 && follows == 1 && Buggy == 21 {1}
                    else if case == 23 && follows == 0 && Buggy == 29 {1} else {0};
            }
            invariant NoStaleLine: line == 0 || case == 1 || case == 13 || case == 14 || case == 15;
            invariant CurrentMovesAllowed: phase <= 1 || provider > 1
                || (case > 1 && 13 > case) || case > 15 || line == 1;
            invariant RefusalChangesNoRotation: committed == 0
                || case == 1 || case == 13 || case == 14 || case == 15;
            invariant CurrentRotationCommitted: phase <= 1 || provider > 1 || follows == 1
                || (case > 1 && 13 > case) || case > 15 || committed == 1;
            invariant FollowingDoesNotRotate: follows == 0 || committed == 0;
            invariant UnmeasuredNeverMoves: provider == 1 || (line == 0 && committed == 0);
        }
    }
}
