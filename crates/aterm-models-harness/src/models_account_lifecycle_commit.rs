// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Current process observations either refuse or recover coherent lifecycle records.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_lifecycle_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountLifecycleCommit {
            const Buggy = 0;
            var phase = 0; var provider = 1; var operation = 1; var fault = 1;
            var accepted = 0; var guarded = 1; var coherent = 1;
            var once = 1; var preserved = 1; var notification = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) { provider = provider + 1; }
            action AnotherOperation when (phase == 0 && 7 > operation) { operation = operation + 1; }
            action AnotherFault when (phase == 0 && 5 > fault) { fault = fault + 1; }
            action Record when (phase == 0) {
                phase = 1;
                accepted = if fault == 1 || (fault == 4 && (operation == 1 || operation == 3 || operation == 4 || operation == 7)) {
                    if Buggy == 1 {0} else {1}
                } else { if Buggy == 2 {1} else {0} };
                guarded = if Buggy == 3 {0} else {1};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                preserved = if Buggy == 6 {0} else {1};
                notification = if Buggy == 7 {0} else {1};
            }
            invariant AvailableWhenRecordsWritable: phase == 0 || (fault > 1 && (fault == 2 || fault == 3 || fault == 5 || operation == 2 || operation == 5 || operation == 6)) || accepted == 1;
            invariant DurableAcknowledgment: fault == 1 || (fault == 4 && (operation == 1 || operation == 3 || operation == 4 || operation == 7)) || accepted == 0;
            invariant RefusalOrPendingFence: guarded == 1;
            invariant CoherentLifecycleAndHistory: coherent == 1;
            invariant RecoveryDoesNotRepeatTheObservation: once == 1;
            invariant RetainsCredentialsAndPolicy: preserved == 1;
            invariant EventBasedNotification: notification == 1;
        }
    }
}
