// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A prepared cold launch must still name the selected, enrolled login at a
/// current policy observation. Off mode requires a current explicit pin.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_launch_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolLaunchPolicy {
            const Buggy = 0;
            var provider = 1; var off = 0; var case = 1;
            var phase = 0; var returned = 0; var wrote = 0;
            action AnotherProvider when (phase == 0 && provider <= 2) { provider = provider + 1; }
            action OffWithPin when (phase == 0 && off == 0) { off = 1; }
            action AnotherCase when (phase == 0 && case <= 26) { case = case + 1; }
            action Prepare when (phase == 0) { phase = 1; }
            action Finish when (phase == 1) {
                phase = 2;
                returned = if case == 1 {if Buggy == 1 {0} else {1}}
                    else if case == 12 {if Buggy == 12 {0} else {1}}
                    else if case == 13 {if Buggy == 13 {0} else {1}}
                    else if case == 14 {if Buggy == 14 {0} else {1}}
                    else if case == 25 {if Buggy == 25 {0} else {1}}
                    else if case == 20 && provider > 1 {1}
                    else if case == 22 && off == 0 {if Buggy == 22 {0} else {1}}
                    else if (case == 2 && Buggy == 2) || (case == 3 && Buggy == 3)
                        || (case == 4 && Buggy == 4) || (case == 5 && Buggy == 5)
                        || (case == 6 && Buggy == 6) || (case == 7 && Buggy == 7)
                        || (case == 8 && Buggy == 8) || (case == 9 && Buggy == 9)
                        || (case == 10 && Buggy == 10) || (case == 11 && Buggy == 11)
                        || (case == 15 && Buggy == 15) || (case == 16 && Buggy == 16)
                        || (case == 17 && Buggy == 17) || (case == 18 && Buggy == 18)
                        || (case == 19 && Buggy == 19) || (case == 20 && Buggy == 20)
                        || (case == 21 && Buggy == 21) || (case == 22 && Buggy == 28)
                        || (case == 23 && Buggy == 23) || (case == 24 && Buggy == 24)
                        || (case == 26 && Buggy == 26) || (case == 27 && Buggy == 27) {1} else {0};
                wrote = if Buggy == 29 {1} else {0};
            }
            invariant NoStaleLaunch: returned == 0 || case == 1 || case == 12 || case == 13
                || case == 14 || case == 25 || (case == 20 && provider > 1)
                || (case == 22 && off == 0);
            invariant CurrentLaunchAllowed: phase <= 1 || returned == 1
                || (case > 1 && 12 > case) || (case > 14 && 20 > case)
                || (case == 20 && provider == 1) || case == 21 || (case == 22 && off == 1)
                || case == 23 || case == 24 || case > 25;
            invariant ValidationDoesNotWrite: wrote == 0;
        }
    }
}
