// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two reads of retained completion data. Each read checks the full request
/// and observation, and owns a successful durability barrier before returning
/// it. This is not a model of host execution or receipt/service cleanup.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_completion_recovery_model() -> Model {
    aterm_spec::ty_model! {
        NativeCompletionRecovery {
            const Buggy = 0;
            var fault = 0; var rollback = 0; var wrapper = 0;
            var dry = 0; var barrier_failed = 0; var attempts = 0;
            var invalid = 0; var unpublished = 0; var dry_result = 0;
            var replayed = 0; var lost = 0;
            action NextFault when (attempts == 0 && fault <= 20) { fault = fault + 1; }
            action Rollback when (attempts == 0 && rollback == 0) { rollback = 1; }
            action Wrapper when (attempts == 0 && wrapper == 0) { wrapper = 1; }
            action Dry when (dry == 0 && attempts <= 1) { dry = 1; }
            action Live when (dry == 1 && attempts <= 1) { dry = 0; }
            action FailBarrier when (barrier_failed == 0 && attempts <= 1) { barrier_failed = 1; }
            action RepairBarrier when (barrier_failed == 1 && attempts <= 1) { barrier_failed = 0; }
            action Attempt when (attempts <= 1) {
                attempts = attempts + 1;
                invalid = if 2 <= fault && Buggy == 1 && dry == 0 && barrier_failed == 0
                    {1} else {invalid};
                unpublished = if fault <= 1 && barrier_failed == 1 && Buggy == 2 && dry == 0
                    {1} else {unpublished};
                dry_result = if fault <= 1 && dry == 1 && Buggy == 3 && barrier_failed == 0
                    {1} else {dry_result};
                replayed = if fault <= 1 && dry == 0 && barrier_failed == 0 && Buggy == 4
                    {1} else {replayed};
                lost = if fault <= 1 && dry == 0 && barrier_failed == 0 && Buggy == 5
                    {1} else {lost};
            }
            invariant ExactCompletion: invalid == 0;
            invariant DurableCompletion: unpublished == 0;
            invariant DryRunIsInert: dry_result == 0;
            invariant NoReplay: replayed == 0;
            invariant ValidCompletionRetained: lost == 0;
            bound Finite: 0 <= fault && fault <= 21
                && 0 <= rollback && rollback <= 1 && 0 <= wrapper && wrapper <= 1
                && 0 <= dry && dry <= 1 && 0 <= barrier_failed && barrier_failed <= 1
                && 0 <= attempts && attempts <= 2
                && 0 <= invalid && invalid <= 1 && 0 <= unpublished && unpublished <= 1
                && 0 <= dry_result && dry_result <= 1 && 0 <= replayed && replayed <= 1
                && 0 <= lost && lost <= 1;
        }
    }
}
