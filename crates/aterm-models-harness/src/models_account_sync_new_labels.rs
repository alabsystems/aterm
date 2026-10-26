// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Additive provider sync admits new labels only without retained authority or a running sign-in.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sync_new_labels_model() -> Model {
    aterm_spec::ty_model! {
        AccountSyncNewLabels {
            const Buggy = 0;
            var case = 1; var phase = 0;
            var accepted = 0; var published = 0; var authority = 0;
            action AnotherCase when (phase == 0 && case <= 31) { case = case + 1; }
            action Sync when (phase == 0) {
                phase = 1;
                accepted = if case == 1 {if Buggy == 1 {0} else {1}}
                    else if case == 2 {if Buggy == 2 {1} else {0}}
                    else if case == 3 {if Buggy == 3 {1} else {0}}
                    else if case == 4 {if Buggy == 4 {1} else {0}}
                    else if case == 5 {if Buggy == 5 {1} else {0}}
                    else if case == 6 {if Buggy == 6 {1} else {0}}
                    else if case == 7 {if Buggy == 7 {1} else {0}}
                    else if case == 8 {if Buggy == 8 {1} else {0}}
                    else if case == 9 {if Buggy == 9 {1} else {0}}
                    else if case == 10 {if Buggy == 10 {1} else {0}}
                    else if case == 11 {if Buggy == 11 {1} else {0}}
                    else if case == 12 {if Buggy == 12 {1} else {0}}
                    else if case == 13 {if Buggy == 13 {1} else {0}}
                    else if case == 14 {if Buggy == 14 {1} else {0}}
                    else if case == 15 {if Buggy == 15 {1} else {0}}
                    else if case == 16 {if Buggy == 16 {1} else {0}}
                    else if case == 17 {if Buggy == 17 {1} else {0}}
                    else if case == 18 {if Buggy == 18 {1} else {0}}
                    else if case == 19 {if Buggy == 19 {0} else {1}}
                    else if case == 20 {if Buggy == 20 {0} else {1}}
                    else if case == 21 {if Buggy == 21 {0} else {1}}
                    else if case == 22 {if Buggy == 22 {0} else {1}}
                    else if case == 23 {if Buggy == 23 {0} else {1}}
                    else if case == 24 {if Buggy == 24 {0} else {1}}
                    else if case == 25 {if Buggy == 25 {0} else {1}}
                    else if case == 26 {if Buggy == 26 {1} else {0}}
                    else if case == 27 {if Buggy == 27 {0} else {1}}
                    else if case == 28 {if Buggy == 28 {0} else {1}}
                    else if case == 29 {if Buggy == 29 {0} else {1}}
                    else if case == 30 {if Buggy == 30 {1} else {0}}
                    else if case == 31 {if Buggy == 31 {1} else {0}}
                    else if case == 32 {if Buggy == 32 {0} else {1}} else {0};
                published = if case == 1 || case == 20 || case == 21 || case == 24 || case == 28 || case == 32 {1}
                    else if Buggy == 33 || (Buggy == 35 && case == 19) {1} else {0};
                authority = if case == 24 {if Buggy == 36 {0} else {1}}
                    else if Buggy == 34 {1} else {0};
            }
            invariant NoUnexpectedAdmission: accepted == 0 || case == 1
                || (case > 18 && 26 > case) || (case > 26 && 30 > case) || case == 32;
            invariant NewRowsAdmitted: phase == 0 || accepted == 1 || (case > 1 && 19 > case)
                || case == 26 || case == 30 || case == 31;
            invariant RefusalPreservesRoster: accepted == 1 || published == 0;
            invariant NoopPreserved: case == 1 || case == 20 || case == 21 || case == 24
                || case == 28 || case == 32 || published == 0;
            invariant NoExtraAuthority: case == 24 || authority == 0;
            invariant RecoveryCompletes: phase == 0 || 24 > case || case > 24 || authority == 1;
        }
    }
}
