// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// One exec-receipt publication with or without a prior attempt. Writer
/// refusal differs from a write error, which may leave new bytes visible.
/// This supplies no execution grant, process-death or account-reclaim proof.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exec_publication_model() -> Model {
    aterm_spec::ty_model! {
        ExecPublication {
            const Buggy = 0;
            var prior = 0; var fault = 0; var done = 0;
            var entered = 0; var memory = 0; var disk = 0; var acknowledged = 0;
            action PriorAttempt when (done == 0 && prior == 0 && fault == 0) {
                prior = 1; memory = 1; disk = 1;
            }
            // Store busy, sign-in busy, no pool commit, disabled policy,
            // pre-rename error, post-rename error, successful publication.
            action NextFault when (done == 0 && fault <= 5) { fault = fault + 1; }
            action Publish when (done == 0) {
                done = 1;
                entered = if fault > 3 {1} else {0};
                memory = if Buggy == 1 {1}
                    else if Buggy == 2 && fault > 3 && fault <= 5 {prior}
                    else if Buggy == 3 && fault <= 3 {0}
                    else if prior == 1 || fault > 3 {1} else {0};
                disk = if prior == 1 || fault > 4 {1} else {0};
                acknowledged = if Buggy == 5 {0}
                    else if fault == 6 || (Buggy == 4 && fault > 3) {1} else {0};
            }
            invariant RefusalPreservesHistory: done == 0 || entered == 1 || memory == prior;
            invariant AttemptRetained: entered == 0 || memory == 1;
            invariant PriorRetained: prior == 0 || memory == 1;
            invariant VisibleWriteCovered: disk == 0 || memory == 1;
            invariant DurableReturn: acknowledged == 0 || fault == 6;
            invariant Available: done == 0 || fault <= 5 || acknowledged == 1;
            bound Finite: 0 <= prior && prior <= 1 && 0 <= fault && fault <= 6
                && 0 <= done && done <= 1 && 0 <= entered && entered <= 1
                && 0 <= memory && memory <= 1 && 0 <= disk && disk <= 1
                && 0 <= acknowledged && acknowledged <= 1;
        }
    }
}
