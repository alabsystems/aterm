// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Source admission spans only intent persistence; interrupted replay needs
/// the retained intent and account writer, never a fresh source grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_receipt_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourceReceipt {
            const Buggy = 0;
            var scenario = 0; var done = 0;
            var called = 0; var accepted = 0; var pending = 0;
            var installed = 0; var recovered = 0; var early = 0;
            action Scenario when (done == 0 && 5 > scenario) { scenario = scenario + 1; }
            action Publish when (done == 0) {
                done = 1;
                called = if scenario == 1 || scenario == 2 {if Buggy == 1 {1} else {0}} else {1};
                accepted = if scenario == 0 {1} else if Buggy == 2 && scenario == 3 {1} else {0};
                pending = if scenario == 4 || scenario == 5 {if Buggy == 3 {0} else {1}} else {0};
                installed = if scenario == 0 {1} else if Buggy == 4 && scenario == 1 {1} else {0};
                recovered = if scenario == 4 || scenario == 5 {if Buggy == 5 {0} else {1}} else {0};
                early = if Buggy == 6 {1} else {0};
            }
            invariant RefusalBeforeSave: done == 0 || (scenario > 2 || scenario == 0) || called == 0;
            invariant CallbackFailureReported: done == 0 || 3 > scenario || scenario > 3 || accepted == 0;
            invariant SavedFailureRetained: done == 0 || 4 > scenario || pending == 1;
            invariant RefusedSourceUnchanged: done == 0 || 1 > scenario || scenario > 1 || installed == 0;
            invariant RecoveryWithoutSource: done == 0 || 4 > scenario || recovered == 1;
            invariant ReplayAfterGuard: early == 0;
            bound ReceiptDomain: scenario <= 5 && done <= 1 && called <= 1 && accepted <= 1
                && pending <= 1 && installed <= 1 && recovered <= 1 && early <= 1;
        }
    }
}
