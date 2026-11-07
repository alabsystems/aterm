// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded reads of positive no-execution evidence, with per-call publication
/// and validation failures. Host execution and service receipt closure are separate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_noexec_recovery_model() -> Model {
    aterm_spec::ty_model! {
        NativeNoexecRecovery {
            const Buggy = 0;
            var fault = 0; var expired = 0;
            var dry = 0; var barrier_failed = 0; var attempts = 0;
            var invalid = 0; var unpublished = 0; var dry_result = 0;
            var replayed = 0; var lost = 0;
            action NextFault when (attempts == 0 && fault <= 14) { fault = fault + 1; }
            action Expire when (attempts == 0 && expired == 0) { expired = 1; }
            action Dry when (attempts <= 1 && dry == 0) { dry = 1; }
            action Live when (attempts <= 1 && dry == 1) { dry = 0; }
            action FailBarrier when (attempts <= 1 && barrier_failed == 0) { barrier_failed = 1; }
            action RepairBarrier when (attempts <= 1 && barrier_failed == 1) { barrier_failed = 0; }
            action Attempt when (attempts <= 1) {
                attempts = attempts + 1;
                invalid = if 2 <= fault && Buggy == 1 && dry == 0 && barrier_failed == 0
                    {1} else {invalid};
                unpublished = if fault <= 1 && Buggy == 2 && dry == 0 && barrier_failed == 1
                    {1} else {unpublished};
                dry_result = if fault <= 1 && Buggy == 3 && dry == 1 && barrier_failed == 0
                    {1} else {dry_result};
                replayed = if fault <= 1 && Buggy == 4 && dry == 0 && barrier_failed == 0
                    {1} else {replayed};
                lost = if fault <= 1 && Buggy == 5 && dry == 0 && barrier_failed == 0
                    {1} else {lost};
            }
            invariant PositiveExactEvidence: invalid == 0;
            invariant DurableNoexec: unpublished == 0;
            invariant DryRunIsInert: dry_result == 0;
            invariant NoReplay: replayed == 0;
            invariant PositiveResultRetained: lost == 0;
            bound Finite: 0 <= fault && fault <= 15 && 0 <= expired && expired <= 1
                && 0 <= dry && dry <= 1 && 0 <= barrier_failed && barrier_failed <= 1
                && 0 <= attempts && attempts <= 2 && 0 <= invalid && invalid <= 1
                && 0 <= unpublished && unpublished <= 1 && 0 <= dry_result && dry_result <= 1
                && 0 <= replayed && replayed <= 1 && 0 <= lost && lost <= 1;
        }
    }
}
