// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Account initiation and observed completion require the policy read by the caller.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountPolicy {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var pending = 0; var moved = 0; var written = 0;
            var restarted = 0; var notified = 0;
            action AnotherCase when (phase == 0 && case <= 11) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                pending = if (Buggy == 3 && case == 2) || case == 1 || case > 8 {0} else {1};
                moved = if (Buggy == 2 && case == 5) {1}
                    else {if (Buggy == 5 && case == 1) || (Buggy == 6 && case == 9) {0}
                        else {if case == 1 || case == 9 || case == 10 {1} else {0}}};
                written = if Buggy == 1 && case == 2 {1}
                    else {if case == 1 || case == 9 || case == 10 {1} else {0}};
                restarted = if Buggy == 4 && case == 11 {1}
                    else {if Buggy == 8 && case == 10 {0} else {if case == 10 {1} else {0}}};
                notified = if Buggy == 7 && case == 2 {1}
                    else {if case == 1 || case == 9 {1} else {0}};
            }
            invariant ChangedPolicyIsQuiet: phase <= 1 || case == 1 || case > 4 || (written == 0 && moved == 0);
            invariant ChangedContextIsQuiet: phase <= 1 || case <= 4 || case > 8 || (written == 0 && moved == 0);
            invariant PendingSurvives: phase <= 1 || case <= 1 || case > 8 || pending == 1;
            invariant AdmissionBeforeRestart: phase <= 1 || case <= 10 || (restarted == 0 && written == 0 && moved == 0);
            invariant CurrentProgress: phase <= 1 || (case > 1 && case <= 8) || case > 10 || (pending == 0 && moved == 1 && written == 1);
            invariant NotificationAfterWrite: notified == 0 || (written == 1 && moved == 1);
            invariant CurrentInitiates: phase <= 1 || case <= 9 || case > 10 || restarted == 1;
        }
    }
}
