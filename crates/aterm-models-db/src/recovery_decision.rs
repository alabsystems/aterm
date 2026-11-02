// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Recovery notices for distinct decisions remain independently visible. A
/// same-day retry appends nothing; copying an old notice into a new day cannot
/// erase a current notice retained in the three-day window.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recovery_decision_model() -> Model {
    aterm_spec::ty_model! {
        DbRecoveryDecisionIsolation {
            const Buggy = 0;
            var current = 0; var old = 0; var retried = 0; var day = 0;
            var visible = 0; var retained = 0; var repaired = 0; var remote = 0;
            action Current when (current == 0 && day == 0) {
                current = 1;
                visible = if Buggy == 1 && old == 1 {1} else {old + 1};
                retained = retained + 1;
            }
            action Old when (old == 0 && day == 0) {
                old = 1;
                visible = if Buggy == 1 && current == 1 {1} else {current + 1};
                retained = retained + 1;
            }
            action Retry when (current == 1 && old == 1 && retried == 0 && day == 0) {
                retried = 1; retained = retained + if Buggy == 2 {1} else {0};
            }
            action NextDay when (retried == 1 && day == 0) {
                day = 1; retained = retained + 1;
                visible = if Buggy == 3 {1} else {current + old};
            }
            action Repair when (day == 1 && repaired == 0) {
                repaired = 1; remote = if Buggy == 4 {1} else {visible};
            }
            invariant EveryDecision: visible == current + old;
            invariant RetryIsIdempotent: retained == current + old + day;
            invariant RepairPreservesDecisions: repaired == 0 || remote == visible;
        }
    }
}
