// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Empty login directories are retired only after the local receipt completes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sync_cleanup_model() -> Model {
    aterm_spec::ty_model! {
        AccountSyncCleanup {
            const Buggy = 0;
            var cut = 0; var done = 0; var completed = 0; var retained = 1;
            action Cut when (done == 0 && 7 > cut) { cut = cut + 1; }
            action Sync when (done == 0) {
                done = 1;
                completed = if cut == 7 {1} else {0};
                retained = if Buggy == 2 || (7 > cut && Buggy == 0) {1} else {0};
            }
            invariant InterruptedRetainsDirectory: completed == 1 || retained == 1;
            invariant CompletedCleansDirectory: done == 0 || completed == 0 || retained == 0;
            bound CutDomain: 0 <= cut && cut <= 7;
        }
    }
}
