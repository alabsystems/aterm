// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Explicit provider realms distinguish accounts during additive roster sync;
/// login methods and Codex aliases retain their existing identity semantics.
/// This finite scenario contract does not quantify over arbitrary identity strings.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_provider_realm_model() -> Model {
    aterm_spec::ty_model! {
        AccountProviderRealm {
            const Buggy = 0;
            var case = 0; var phase = 0; var accepted = 0;
            var added = 0; var kept = 0; var preserved = 1; var repeat = 1;
            action AnotherCase when (phase == 0 && case <= 20) { case = case + 1; }
            action Sync when (phase == 0) {
                phase = 1;
                accepted = if case == 11 || case == 19 || case == 20 {if Buggy == 2 {1} else {0}}
                    else if case == 4 && Buggy == 8 {0} else {1};
                added = if case == 16 {2} else if case == 0 || case == 12 || case == 14 || case == 17 {
                    if case == 0 && Buggy == 1 {0} else {1}
                } else if case == 5 && Buggy == 4 {1} else {0};
                kept = if case == 1 || case == 2 || (case > 4 && case <= 8) || case == 10 || case == 15 || case == 18 || case == 21 {
                    if (case == 7 && Buggy == 5) || (case == 10 && Buggy == 6) {0} else {1}
                } else {0};
                preserved = if case == 2 && Buggy == 3 {0} else {1};
                repeat = if case == 0 && Buggy == 7 {0} else {1};
            }
            invariant Admission: phase == 0 || accepted == if case == 11 || case == 19 || case == 20 {0} else {1};
            invariant Additions: phase == 0 || added == if case == 16 {2} else if case == 0 || case == 12 || case == 14 || case == 17 {1} else {0};
            invariant KeptReports: phase == 0 || kept == if case == 1 || case == 2 || (case > 4 && case <= 8) || case == 10 || case == 15 || case == 18 || case == 21 {1} else {0};
            invariant ExistingPreserved: preserved == 1;
            invariant RepeatStable: repeat == 1;
        }
    }
}
