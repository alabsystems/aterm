// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Fixed-path login publication recovers coherent metadata without consuming another generation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_local_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountLocalCommit {
            const Buggy = 0;
            var phase = 0; var provider = 1; var operation = 1; var fault = 1;
            var recovered = 0; var fenced = 1; var coherent = 1;
            var once = 1; var stable = 1; var preserved = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) {provider = provider + 1;}
            action AnotherOperation when (phase == 0 && 2 > operation) {operation = operation + 1;}
            action AnotherFault when (phase == 0 && 8 > fault) {fault = fault + 1;}
            action Commit when (phase == 0) {
                phase = 1;
                recovered = if fault == 5 || fault == 6 {if Buggy == 2 {1} else {0}}
                    else {if Buggy == 1 {0} else {1}};
                fenced = if Buggy == 3 {0} else {1};
                coherent = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                stable = if Buggy == 6 {0} else {1};
                preserved = if Buggy == 7 {0} else {1};
            }
            invariant RecoverableCompletes: phase == 0 || fault == 5 || fault == 6 || recovered == 1;
            invariant ConflictsRefuse: phase == 0 || (5 > fault || fault > 6) || recovered == 0;
            invariant PendingFencesReaders: fenced == 1;
            invariant RecordsStayTogether: coherent == 1;
            invariant GenerationAndHistoryOnce: once == 1;
            invariant RetryIsStable: stable == 1;
            invariant UnrelatedFilesPreserved: preserved == 1;
        }
    }
}
