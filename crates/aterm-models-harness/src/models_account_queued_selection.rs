// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Queued selection admission preserves intent through stale work and local refusals.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_queued_selection_model() -> Model {
    aterm_spec::ty_model! {
        AccountQueuedSelection {
            const Buggy = 0;
            var phase = 0; var provider = 1; var case = 1;
            var outcome = 0; var retained = 1; var untouched = 1; var retry = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) {provider = provider + 1;}
            action AnotherCase when (phase == 0 && 16 > case) {case = case + 1;}
            action Check when (phase == 0 && (10 > case || case > 12)) {
                phase = 1;
                outcome = if case == 1 {if Buggy == 2 {0} else {1}}
                    else {if Buggy == 1 {1} else {0}};
                retained = if Buggy == 5 {0} else {1};
                untouched = if Buggy == 6 {0} else {1};
                retry = if Buggy == 7 {0} else {1};
            }
            action Lock when (phase == 0 && case > 9 && 13 > case) {
                phase = 1;
                outcome = if case == 10 {if Buggy == 8 {0} else {1}}
                    else if case == 11 {if Buggy == 3 {4} else {2}}
                    else {if Buggy == 4 {4} else {3}};
                retained = if Buggy == 5 {0} else {1};
                untouched = if Buggy == 6 {0} else {1};
                retry = if Buggy == 7 {0} else {1};
            }
            invariant CurrentSelectionRuns: phase == 0 || case > 1 || outcome == 1;
            invariant StaleOrUnreadableDoesNotRun: phase == 0 || case == 1 || (case > 9 && 13 > case) || outcome == 0;
            invariant HealthyLockRuns: phase == 0 || 10 > case || case > 10 || outcome == 1;
            invariant BusyLockWaits: phase == 0 || 11 > case || case > 11 || outcome == 2;
            invariant UnreadableLockFences: phase == 0 || 12 > case || case > 12 || outcome == 3;
            invariant KeepsCurrentIntent: retained == 1;
            invariant AdmissionEditsNoRecords: untouched == 1;
            invariant RepairOrFreshSnapshotWorks: retry == 1;
        }
    }
}
