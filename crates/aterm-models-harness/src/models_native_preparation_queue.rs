// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Native preparation scheduling: unseen and older mature accounts precede
/// repeated work, while pruning preserves other homes and their cooldowns.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_preparation_queue_model() -> Model {
    aterm_spec::ty_model! {
        NativePreparationQueue {
            const Buggy = 0;
            var phase = 0; var picked = 0; var count = 0;
            var a = 0; var b = 0; var other = 0;
            action Initial when (phase == 0) {
                phase = 1; picked = if Buggy == 2 {2} else {1}; count = 1;
                a = if Buggy == 2 {0} else {10}; b = if Buggy == 2 {10} else {0};
            }
            action Sparse when (phase == 1) {
                phase = 2; picked = if Buggy == 1 {1} else {2};
                a = if Buggy == 1 {70} else {10}; b = if Buggy == 1 {0} else {70};
                count = if Buggy == 1 {1} else {2};
            }
            action Late when (phase == 2) {
                phase = 3; picked = if Buggy == 3 {2} else {1};
                a = if Buggy == 3 {10} else {200}; b = if Buggy == 3 {200} else {70};
            }
            action Prune when (phase == 1) {
                phase = 4; picked = 2; a = if Buggy == 4 {10} else {0}; b = 70;
                count = if Buggy == 4 {2} else {1};
            }
            action OtherHome when (phase == 1) {
                phase = 5; picked = 1; other = 11;
                a = if Buggy == 5 {0} else {10}; count = if Buggy == 5 {1} else {2};
            }
            action Grow when (phase == 1) {
                phase = 6; picked = if Buggy == 6 {0} else {2};
                count = if Buggy == 6 {1} else {2}; b = if Buggy == 6 {0} else {11};
            }
            action Cooldown when (phase == 1) {
                phase = 7; picked = if Buggy == 7 {1} else {0};
                a = if Buggy == 7 {69} else {10};
            }
            action Boundary when (phase == 7) {
                phase = 8; picked = if Buggy == 8 {0} else {1};
                a = if Buggy == 8 {10} else {70};
            }
            action Future when (phase == 1) {
                phase = 9; picked = if Buggy == 9 {1} else {0};
                a = if Buggy == 9 {9} else {10};
            }
            action Empty when (phase == 1) {
                phase = 10; picked = 0; a = if Buggy == 4 {10} else {0};
                count = if Buggy == 4 {1} else {0};
            }
            action Disappear when (phase == 1) {
                phase = 11; picked = 0; a = if Buggy == 10 {0} else {10};
                count = if Buggy == 10 {0} else {1};
            }
            action Reappear when (phase == 11) {
                phase = 12; picked = if Buggy == 10 {1} else {0};
                a = if Buggy == 10 {12} else {10}; count = 1;
            }
            invariant StableTie: 1 > phase || phase > 1 || picked == 1;
            invariant UnseenFirst: 2 > phase || phase > 2 || picked == 2;
            invariant OldestMatureFirst: 3 > phase || phase > 3 || picked == 1;
            invariant RetiredSlotsReclaimed: 4 > phase || phase > 4 || (a == 0 && b == 70 && count == 1);
            invariant HomesAreIndependent: 5 > phase || phase > 5 || (a == 10 && other == 11 && count == 2);
            invariant NewAccountsRemainEligible: 6 > phase || phase > 6 || (count == 2 && picked == 2);
            invariant NoEarlyRetry: 7 > phase || phase > 7 || (picked == 0 && a == 10);
            invariant ExactBoundary: 8 > phase || phase > 8 || (picked == 1 && a == 70);
            invariant FutureStaysCooling: 9 > phase || phase > 9 || (picked == 0 && a == 10);
            invariant EmptyReleasesOwnSlots: 10 > phase || phase > 10 || count == 0;
            invariant CoolingSurvivesAbsence: 11 > phase || phase > 11 || (a == 10 && count == 1);
            invariant ReturningStaysCooling: 12 > phase || phase > 12 || (a == 10 && picked == 0);
        }
    }
}
