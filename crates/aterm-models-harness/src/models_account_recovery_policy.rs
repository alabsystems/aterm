// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Each provider recovers a checkpoint only against its current identity and parent binding.
/// Native transitions cover clock rollback, deletion, legacy bindings, schema rejection and
/// unrelated renewals inside the real account store.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_recovery_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountRecoveryPolicy {
            const Buggy = 0;
            var phase = 0; var provider = 1; var case = 1; var recovered = 0;
            action AnotherProvider when (phase == 0 && provider <= 3) { provider = provider + 1; }
            action AnotherCase when (phase == 0 && case <= 30) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Recover when (phase == 1) {
                phase = 2;
                recovered = if case == 31 { if Buggy == 33 {1} else {0} }
                else if case == 27 || case == 28 || case == 29 {
                    if provider == 4 {
                        if (case == 27 && Buggy == 27) || (case == 28 && Buggy == 28)
                            || (case == 29 && Buggy == 31) {1} else {0}
                    } else {
                        if (case == 27 && Buggy == 29) || (case == 28 && Buggy == 30) {0} else {1}
                    }
                } else if case == 1 || case == 9 || case == 19 || case == 20 || case == 22 || case == 30 {
                    if (case == 1 && Buggy == 8) || (case == 9 && Buggy == 9)
                        || (case == 19 && Buggy == 19) || (case == 20 && Buggy == 20)
                        || (case == 22 && Buggy == 22) || (case == 30 && Buggy == 32) {0} else {1} }
                    else if (case == 2 && Buggy == 1) || (case == 3 && Buggy == 2)
                        || (case == 4 && Buggy == 3) || (case == 5 && Buggy == 4)
                        || (case == 6 && Buggy == 5) || (case == 7 && Buggy == 6)
                        || (case == 8 && Buggy == 7)
                        || (case == 10 && Buggy == 10) || (case == 11 && Buggy == 11)
                        || (case == 12 && Buggy == 12) || (case == 13 && Buggy == 13)
                        || (case == 14 && Buggy == 14) || (case == 15 && Buggy == 15)
                        || (case == 16 && Buggy == 16) || (case == 17 && Buggy == 17)
                        || (case == 18 && Buggy == 18) || (case == 21 && Buggy == 21)
                        || (case == 23 && Buggy == 23) || (case == 24 && Buggy == 24)
                        || (case == 25 && Buggy == 25) || (case == 26 && Buggy == 26) {1} else {0};
            }
            invariant AdmissibleRecoveryOnly: phase <= 1 || recovered == 0
                || case == 1 || case == 9 || case == 19 || case == 20 || case == 22 || case == 30
                || ((case == 27 || case == 28 || case == 29) && provider <= 3);
            invariant AdmissibleRecoverySucceeds: phase <= 1
                || (case > 1 && case <= 8) || (case > 9 && case <= 18)
                || case == 21 || case == 31 || (case > 22 && case <= 26)
                || ((case == 27 || case == 28 || case == 29) && provider == 4)
                || recovered == 1;
        }
    }
}
