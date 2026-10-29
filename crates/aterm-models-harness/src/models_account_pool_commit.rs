// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Operational pool writes acknowledge complete records and recover admitted ones once.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolCommit {
            const Buggy = 0;
            var phase = 0; var operation = 1; var fault = 1;
            var accepted = 0; var guarded = 1; var coherent = 1; var once = 1; var preserved = 1; var notification = 1;
            action AnotherOperation when (phase == 0 && 10 > operation) {operation = operation + 1;}
            action AnotherFault when (phase == 0 && 4 > fault) {fault = fault + 1;}
            action Rotate when (phase == 0 && operation == 1) {
                phase = 1;
                accepted = if fault == 1 || fault == 4 {if Buggy == 2 {0} else {1}} else {if Buggy == 1 {1} else {0}};
                guarded = if Buggy == 3 {0} else {1};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                preserved = if Buggy == 6 {0} else {1};
                notification = if Buggy == 7 {0} else {1};
            }
            action Usage when (phase == 0 && operation > 1 && 5 > operation) {
                phase = 1;
                accepted = if fault == 1 || fault == 4 {if Buggy == 2 {0} else {1}} else {if Buggy == 1 {1} else {0}};
                guarded = if Buggy == 3 {0} else {1};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                preserved = if Buggy == 6 {0} else {1};
                notification = if Buggy == 7 {0} else {1};
            }
            action Observe when (phase == 0 && operation > 4 && 10 > operation) {
                phase = 1;
                accepted = if fault == 1 || (fault == 4 && 9 > operation) {if Buggy == 2 {0} else {1}} else {if Buggy == 1 {1} else {0}};
                guarded = if Buggy == 3 {0} else {1};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                preserved = if Buggy == 6 {0} else {1};
                notification = if Buggy == 7 {0} else {1};
            }
            action Take when (phase == 0 && operation == 10) {
                phase = 1;
                accepted = if fault == 1 || fault == 4 {if Buggy == 2 {0} else {1}} else {if Buggy == 1 {1} else {0}};
                guarded = if Buggy == 3 {0} else {1};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                preserved = if Buggy == 6 {0} else {1};
                notification = if Buggy == 7 {0} else {1};
            }
            invariant SuccessfulWritesAvailable: phase == 0 || accepted == 1 || (fault > 1 && (fault == 2 || fault == 3 || operation == 9));
            invariant DurableAcknowledgment: fault == 1 || (fault == 4 && (9 > operation || operation == 10)) || accepted == 0;
            invariant PendingFenceAndAdmission: guarded == 1;
            invariant CoherentRecords: coherent == 1;
            invariant NoDuplicateEventsOrGenerations: once == 1;
            invariant RetainsPolicyAndCredentials: preserved == 1;
            invariant NotificationFollowsEvents: notification == 1;
        }
    }
}
