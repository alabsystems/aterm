// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Original primary custody remains authoritative, but retained recovery
/// liabilities must participate in observation and every later admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resource_recovery_model() -> Model {
    aterm_spec::ty_model! {
        DbResourceRecovery {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var observed = 0;
            var invoked = 0; var used = 0;
            action Observe when (scenario == 0) {
                scenario = 1; observed = if Buggy == 1 {0} else {1}; used = 2;
            }
            action Reserve when (scenario == 0) {
                scenario = 2; accepted = if Buggy == 2 {0} else {1};
                used = if Buggy == 5 {5} else {3};
            }
            action InvalidReserve when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 3 {1} else {0};
            }
            action InvalidDispatch when (scenario == 0) {
                scenario = 4; invoked = if Buggy == 4 {1} else {0};
            }
            invariant OriginalCustodyReadable: 1 > scenario || scenario > 1 || observed == 1;
            invariant ValidProgress: 2 > scenario || scenario > 2 || accepted == 1;
            invariant InvalidLiabilityRefuses: 3 > scenario || scenario > 3 || accepted == 0;
            invariant NoInvalidInvocation: invoked == 0;
            invariant FixedCapacity: used <= 4;
        }
    }
}
