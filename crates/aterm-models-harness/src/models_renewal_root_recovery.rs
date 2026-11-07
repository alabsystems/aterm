// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One root coordinator call over real receipt/native journals. Native host
/// admission and service seal reclamation are separate boundaries.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn renewal_root_recovery_model() -> Model {
    aterm_spec::ty_model! {
        RenewalRootRecovery {
            const Buggy = 0;
            var scenario = 0; var done = 0;
            var prepared = 0; var driven = 0; var published = 0;
            action NextCase when (done == 0 && scenario <= 12) { scenario = scenario + 1; }
            action Step when (done == 0) {
                done = 1;
                prepared = if scenario == 0 || scenario == 6 || scenario == 7 || scenario == 9
                    || scenario == 10 || scenario == 11 || scenario == 13 || Buggy == 1 {1} else {0};
                driven = if scenario == 0 || scenario == 11 || Buggy == 2 {1} else {0};
                published = if Buggy == 3 {0}
                    else if scenario == 0 || scenario == 1 || scenario == 2 || scenario == 9
                        || scenario == 10 || scenario == 11 || Buggy == 4 {1} else {0};
            }
            invariant PrepareOnlyLive: prepared == 0 || scenario == 0 || scenario == 6
                || scenario == 7 || scenario == 9 || scenario == 10 || scenario == 11 || scenario == 13;
            invariant DriveOnlyAuthorized: driven == 0 || scenario == 0 || scenario == 11;
            invariant RetainedCompletionProgress: done == 0
                || (scenario > 2 && scenario <= 8) || scenario == 12 || scenario == 13 || published == 1;
            invariant NoInventedCompletion: published == 0 || scenario <= 2
                || scenario == 9 || scenario == 10 || scenario == 11;
            bound Finite: 0 <= scenario && scenario <= 13 && 0 <= done && done <= 1
                && 0 <= prepared && prepared <= 1 && 0 <= driven && driven <= 1
                && 0 <= published && published <= 1;
        }
    }
}
