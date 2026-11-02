// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Separate replicas restore authenticated reads without granting writer succession.
/// Each action runs an independent durable fixture, including the original actor's
/// own retained history. Recovery is not a repair of missing writer custody.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_recovery_model() -> Model {
    aterm_spec::ty_model! {
        DbTableRecovery {
            const Buggy = 0;
            var scenario = 0; var readable = 0; var authored = 0;
            var original = 1; var promoted = 0;
            action Receive when (scenario == 0) {
                scenario = 1; readable = if Buggy == 2 {0} else {1};
                original = if Buggy == 3 {0} else {1};
            }
            action Reopen when (scenario == 0) {
                scenario = 2; readable = if Buggy == 2 {0} else {1};
            }
            action AuthorEmpty when (scenario == 0) {
                scenario = 3; authored = if Buggy == 1 {1} else {0};
            }
            action AuthorReceived when (scenario == 0) {
                scenario = 4; readable = 1; authored = if Buggy == 1 {1} else {0};
            }
            action AuthorRestart when (scenario == 0) {
                scenario = 5; readable = 1; authored = if Buggy == 1 {1} else {0};
            }
            action Promote when (scenario == 0) {
                scenario = 6; promoted = if Buggy == 4 {1} else {0};
            }
            action Corrupt when (scenario == 0) {
                scenario = 7; readable = if Buggy == 5 {1} else {0};
            }
            action WrongScope when (scenario == 0) {
                scenario = 8; readable = if Buggy == 6 {1} else {0};
            }
            action ChangedControl when (scenario == 0) {
                scenario = 9; readable = if Buggy == 7 {1} else {0};
            }
            invariant RecoveryProgress: 1 > scenario || scenario > 2 || readable == 1;
            invariant AuthorshipFenced: authored == 0;
            invariant OriginalUntouched: original == 1;
            invariant PromotionFenced: promoted == 0;
            invariant CorruptionRefused: 7 > scenario || scenario > 7 || readable == 0;
            invariant ScopeRefused: 8 > scenario || scenario > 8 || readable == 0;
            invariant ChangedObservationRefused: 9 > scenario || readable == 0;
        }
    }
}
