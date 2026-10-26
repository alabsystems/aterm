// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Pool reads heal admitted commits and refuse stale or untrusted launch decisions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_recovery_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolRecovery {
            const Buggy = 0;
            var phase = 0; var provider = 1; var case = 1;
            var outcome = 0; var coherent = 1; var once = 1; var preserved = 1; var unpolled = 1;
            action AnotherProvider when (phase == 0 && 3 > provider) {provider = provider + 1;}
            action AnotherCase when (phase == 0 && 9 > case) {case = case + 1;}
            action Observe when (phase == 0) {
                phase = 1;
                outcome = if 3 > case || case == 9 {if Buggy == 2 {0} else {2}}
                    else if case == 5 {if Buggy == 8 {0} else {1}}
                    else {if Buggy == 1 {2} else {if Buggy == 3 {1} else {0}}};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                preserved = if Buggy == 6 {0} else {1};
                unpolled = if Buggy == 7 {0} else {1};
            }
            invariant RecoveredLaunchAllowed: phase == 0 || (case > 2 && 9 > case) || outcome == 2;
            invariant NoUnsafeLaunch: outcome == 0 || outcome == 1 || 3 > case || case == 9;
            invariant OnlyIntentionalUnmanaged: outcome == 0 || outcome == 2 || case == 5;
            invariant EmptyPoolRemainsUsable: phase == 0 || 5 > case || case > 5 || outcome == 1;
            invariant CoherentRecoveredSnapshot: coherent == 1;
            invariant GenerationAndLedgerOnce: once == 1;
            invariant ConflictsAndCredentialsPreserved: preserved == 1;
            invariant RefusalBeforeProviderUsage: unpolled == 1;
        }
    }
}
