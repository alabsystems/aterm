// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Each provider's real roster/pool publication, through every receipt replay cut.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_provider_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountProviderCommit {
            const Buggy = 0;
            var provider = 0; var cut = 0; var fault = 0; var phase = 0;
            var completed = 0; var early = 0; var fenced = 1;
            var exact = 1; var preserved = 1; var once = 1;
            action Provider when (phase == 0 && 3 > provider) { provider = provider + 1; }
            action Cut when (phase == 0 && 5 > cut) { cut = cut + 1; }
            action Fault when (phase == 0 && 2 > fault) { fault = fault + 1; }
            action Recover when (phase == 0) {
                phase = 1;
                completed = if Buggy == 1 {1} else {if fault == 0 {1} else {0}};
                early = if Buggy == 2 {1} else {0};
                fenced = if Buggy == 3 {0} else {1};
                exact = if Buggy == 4 {0} else {1};
                preserved = if Buggy == 5 {0} else {1};
                once = if Buggy == 6 {0} else {1};
            }
            invariant RecoveryAdmitted: phase == 0 || (completed == 1 && fault == 0) || (completed == 0 && fault > 0);
            invariant NoEarlyImages: early == 0;
            invariant PendingFencesReads: fenced == 1;
            invariant ExactPoolIdentity: exact == 1;
            invariant RejectedRecoveryPreserves: preserved == 1;
            invariant ReplayIdempotent: once == 1;
            bound ProviderDomain: 0 <= provider && provider <= 3;
            bound CutDomain: 0 <= cut && cut <= 5;
            bound FaultDomain: 0 <= fault && fault <= 2;
        }
    }
}
