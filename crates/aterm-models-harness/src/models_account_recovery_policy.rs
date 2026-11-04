// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Each provider recovers a checkpoint only against its current identity and parent binding.
/// Native transitions cover clock rollback, deletion, legacy bindings, schema rejection and
/// unrelated renewals inside the real account store. Retired managed lifetime observations
/// cannot fence a new parent; current observations and Codex's raw credential fences still do.
/// Only a committed recovery releases the captured local retry; unrelated holds remain.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_recovery_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountRecoveryPolicy {
            const Buggy = 0;
            var phase = 0; var provider = 1; var case = 1; var recovered = 0;
            var local_hold = 1; var foreign_hold = 1;
            action AnotherProvider when (phase == 0 && provider <= 3) { provider = provider + 1; }
            action AnotherCase when (phase == 0 && case <= 40) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Recover when (phase == 1) {
                phase = 2;
                recovered = if case > 31 {
                    if case == 32 || (case > 34 && case <= 37) {
                        if provider == 2 {
                            if (case == 32 && Buggy == 38) || (case == 35 && Buggy == 42)
                                || (case == 36 && Buggy == 44) || (case == 37 && Buggy == 46) {1} else {0}
                        } else {
                            if (case == 32 && Buggy == 37) || (case == 35 && Buggy == 41)
                                || (case == 36 && Buggy == 43) || (case == 37 && Buggy == 45) {0} else {1}
                        }
                    } else if (case == 33 && Buggy == 39) || (case == 34 && Buggy == 40)
                        || (case == 38 && Buggy == 47) || (case == 39 && Buggy == 48)
                        || (case == 40 && Buggy == 49) || (case == 41 && Buggy == 50) {1} else {0}
                } else if case == 31 { if Buggy == 33 {1} else {0} }
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
                local_hold = if Buggy == 34 {1} else if Buggy == 35 {0}
                    else if case == 1 || case == 9 || case == 19 || case == 20 || case == 22 || case == 30
                        || ((case == 27 || case == 28 || case == 29) && provider <= 3)
                        || ((case == 32 || (case > 34 && case <= 37)) && (2 > provider || provider > 2)) {0} else {1};
                foreign_hold = if Buggy == 36 {0} else {1};
            }
            invariant AdmissibleRecoveryOnly: phase <= 1 || recovered == 0
                || case == 1 || case == 9 || case == 19 || case == 20 || case == 22 || case == 30
                || ((case == 27 || case == 28 || case == 29) && provider <= 3)
                || ((case == 32 || (case > 34 && case <= 37)) && (2 > provider || provider > 2));
            invariant AdmissibleRecoverySucceeds: phase <= 1
                || (case > 1 && case <= 8) || (case > 9 && case <= 18)
                || case == 21 || case == 31 || (case > 22 && case <= 26)
                || ((case == 27 || case == 28 || case == 29) && provider == 4)
                || case == 33 || case == 34 || case > 37
                || ((case == 32 || (case > 34 && case <= 37)) && provider == 2)
                || recovered == 1;
            invariant RecoveryReleasesOwnRetry: phase <= 1 || local_hold + recovered == 1;
            invariant ForeignRetryPreserved: foreign_hold == 1;
            invariant RetiredLifetimeAllowsNewParent: phase <= 1 || provider == 2
                || case <= 31 || case == 33 || case == 34 || case > 37 || recovered == 1;
            invariant CurrentLifetimeBlocksRecovery: phase <= 1
                || case <= 32 || (case > 33 && case <= 37) || case > 40 || recovered == 0;
            invariant ParentFenceSurvivesRetirement: phase <= 1
                || 34 > case || case > 34 || recovered == 0;
            invariant CodexLifetimeFencePreserved: phase <= 1 || 2 > provider || provider > 2
                || case <= 31 || case == 33 || case == 34 || case > 37 || recovered == 0;
            invariant WrongAccountFencePreserved: phase <= 1
                || 41 > case || case > 41 || recovered == 0;
        }
    }
}
