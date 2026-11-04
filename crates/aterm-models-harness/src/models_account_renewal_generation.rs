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
            var recorded = 0; var attempted = 0; var failure_current = 0; var auth_unknown = 0;
            var history_kept = 1; var selectable = 0; var expired = 0; var read_only = 1;
            action AnotherCase when (phase == 0 && recorded == 0 && case <= 15) { case = case + 1; }
            action RecordFailure when (phase == 0 && recorded == 0 && (case == 12 || case == 13)) {
                recorded = 1; failure_current = if Buggy == 20 {0} else {1};
            }
            action Choose when (phase == 0) { phase = 1; }
            action BeginAttempt when (phase == 1 && recorded == 1 && attempted == 0) {
                attempted = 1;
                failure_current = if Buggy == 28 {0} else {failure_current};
                auth_unknown = if Buggy == 29 {1} else {0};
            }
            action Commit when (phase == 1) {
                phase = 2;
                accepted = if case == 5 || case == 6 || case == 9 || case == 14 || case == 15 || case == 16 {
                    if (case == 5 && Buggy == 5) || (case == 6 && Buggy == 6)
                        || (case == 9 && Buggy == 7) || (case == 14 && Buggy == 15)
                        || (case == 15 && Buggy == 17) || (case == 16 && Buggy == 30) {1} else {0}
                } else if case == 3 && Buggy == 19 {0} else {1};
                advanced = if case <= 3 || case == 7 || (case > 9 && case <= 13) {
                    if (case == 1 && Buggy == 1) || (case == 2 && Buggy == 2)
                        || (case == 3 && Buggy == 3) || (case == 7 && Buggy == 4)
                        || (case == 10 && Buggy == 11) || (case == 11 && Buggy == 12)
                        || (case == 12 && Buggy == 13) || (case == 13 && Buggy == 14) {0} else {1}
                } else if (case == 4 && Buggy == 8) || (case == 8 && Buggy == 9)
                    || (case == 16 && Buggy == 32) {1} else {0};
                preserved = if (case == 5 && Buggy == 10) || (case == 14 && Buggy == 16)
                    || (case == 15 && Buggy == 18) || (case == 16 && Buggy == 31) {0} else {1};
                failure_current = if recorded == 1 {if Buggy == 21 {1} else {0}} else {failure_current};
                auth_unknown = if recorded == 1 {if Buggy == 22 {0} else {1}} else {auth_unknown};
                history_kept = if recorded == 1 && Buggy == 23 {0} else {history_kept};
            }
            action ReadAuth when (phase == 2 && recorded == 1) {
                phase = 3; selectable = if Buggy == 24 {0} else {1};
                expired = if Buggy == 25 {1} else {0};
                read_only = if Buggy == 26 {0} else {1};
            }
            action FreshFailure when (phase == 3 && recorded == 1) {
                phase = 4; failure_current = if Buggy == 27 {0} else {1};
                auth_unknown = 0; selectable = 0; expired = 1;
            }
            invariant RenewalAdvances: phase <= 1 || case == 4 || case == 5 || case == 6
                || case == 8 || case == 9 || case > 13 || advanced == 1;
            invariant OtherKeeps: phase <= 1 || case <= 3 || case == 7
                || (case > 9 && case <= 13) || advanced == 0;
            invariant AllowedPublishes: phase <= 1 || case == 5 || case == 6 || case == 9 || case > 13 || accepted == 1;
            invariant Refused: phase <= 1 || case <= 4 || case == 7 || case == 8
                || (case > 9 && case <= 13) || accepted == 0;
            invariant RefusalRetains: accepted == 1 || preserved == 1;
            invariant RecordedFailureApplies: recorded == 0 || phase > 1 || failure_current == 1;
            invariant AttemptKeepsFailure: recorded == 0 || phase > 1 || auth_unknown == 0;
            invariant ReplacedFailureRetired: recorded == 0 || phase <= 1 || phase == 4 || failure_current == 0;
            invariant ReplacementUnknown: recorded == 0 || phase <= 1 || phase == 4 || auth_unknown == 1;
            invariant FailureHistoryRetained: history_kept == 1;
            invariant ReplacementCanServe: phase <= 2 || phase > 3 || (selectable == 1 && expired == 0);
            invariant AuthReadDoesNotWrite: read_only == 1;
            invariant FreshFailureApplies: phase <= 3 || failure_current == 1;
        }
    }
}
