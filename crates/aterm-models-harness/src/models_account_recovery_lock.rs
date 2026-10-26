// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Recovery excludes same- and cross-provider sign-ins through each checkpoint transaction.
/// Native callbacks observe admission and transaction completion, including refusal paths.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_recovery_lock_model() -> Model {
    aterm_spec::ty_model! {
        AccountRecoveryLock {
            const Buggy = 0;
            var case = 1; var phase = 0; var published = 0;
            var overlap = 0; var lock_order = 1; var released = 1; var preserved = 1;
            var visits = 0; var noop_created_lock = 0;
            action AnotherCase when (phase == 0 && case <= 21) { case = case + 1; }
            action Recover when (phase == 0) {
                phase = 1;
                published = if (case == 1 && Buggy == 1) || (case == 2 && Buggy == 2)
                    || (case == 3 && Buggy == 3) || (case == 4 && Buggy == 4)
                    || (case == 5 && Buggy == 5) || (case == 6 && Buggy == 6)
                    || (case == 7 && Buggy == 7) || (case == 8 && Buggy == 8)
                    || (case == 9 && Buggy == 9) || (case == 10 && Buggy == 10)
                    || (case == 11 && Buggy == 11) || (case == 12 && Buggy == 12)
                    || (case == 13 && Buggy == 13) || (case == 14 && Buggy == 14)
                    || (case == 15 && Buggy == 15) || (case == 16 && Buggy == 16)
                    || (case == 17 && Buggy == 17) || (case == 18 && Buggy == 18)
                    || (case == 19 && Buggy == 19) || (case == 20 && Buggy == 20)
                    || (case == 21 && Buggy == 21) || (case == 22 && Buggy == 22)
                    {if case == 1 || case == 2 || case == 15
                    || case == 17 || case == 18 || case == 21 {0} else {1}}
                    else if case == 15 {2}
                    else if case == 1 || case == 2 || case == 17 || case == 18 || case == 21 {1} else {0};
                overlap = if Buggy == 23 {1} else {0};
                lock_order = if Buggy == 24 {0} else {1};
                released = if Buggy == 25 {0} else {1};
                preserved = if Buggy == 26 {0} else {1};
                visits = if Buggy == 27 {0}
                    else if case == 3 || case == 4 || case == 5 || case == 6
                        || case == 7 || case == 8 {0}
                    else if case == 19 {1}
                    else if case == 15 || case == 18 || case == 21 {3} else {2};
                noop_created_lock = if Buggy == 28 {1} else {0};
            }
            invariant PublicationPolicy: phase == 0
                || ((case == 1 || case == 2 || case == 17 || case == 18 || case == 21) && published == 1)
                || (case == 15 && published == 2)
                || ((case > 2 && 15 > case || case == 16 || case == 19 || case == 20 || case == 22) && published == 0);
            invariant SigninExcluded: overlap == 0;
            invariant WriterTakenAfterSignin: lock_order == 1;
            invariant GuardReleased: released == 1;
            invariant EvidencePreserved: preserved == 1;
            invariant GuardObserved: phase == 0 ||
                ((case == 3 || case == 4 || case == 5 || case == 6 || case == 7 || case == 8) && visits == 0)
                || (case == 19 && visits == 1)
                || ((case == 15 || case == 18 || case == 21) && visits == 3)
                || ((case == 1 || case == 2 || case == 9 || case == 10 || case == 11
                    || case == 12 || case == 13 || case == 14 || case == 16 || case == 17
                    || case == 20 || case == 22) && visits == 2);
            invariant NoopHasNoLockArtifact: noop_created_lock == 0;
        }
    }
}
