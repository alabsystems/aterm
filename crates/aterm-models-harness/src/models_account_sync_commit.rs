// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Interrupted sync prefixes recover exact images; conflicting or damaged inputs refuse.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sync_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountSyncCommit {
            const Buggy = 0;
            var phase = 0; var case = 1; var completed = 0;
            var fenced = 1; var preserved = 1; var exact = 1; var once = 1; var epoch = 1;
            action AnotherCase when (phase == 0 && 16 > case) {case = case + 1;}
            action Recover when (phase == 0) {
                phase = 1;
                completed = if 7 > case {if Buggy == 7 {0} else {1}}
                    else {if Buggy == 1 {1} else {0}};
                fenced = if Buggy == 2 {0} else {1};
                preserved = if Buggy == 3 {0} else {1};
                exact = if Buggy == 4 {0} else {1};
                once = if Buggy == 5 {0} else {1};
                epoch = if Buggy == 6 {0} else {1};
            }
            invariant ConflictRefused: completed == 0 || 7 > case;
            invariant ValidPrefixCompletes: phase == 0 || case > 6 || completed == 1;
            invariant PendingFencesReaders: fenced == 1;
            invariant OutsideEditsPreserved: preserved == 1;
            invariant AdmittedImagesOnly: exact == 1;
            invariant ReplayIdempotent: once == 1;
            invariant SpanningReadRefused: epoch == 1;
        }
    }
}
