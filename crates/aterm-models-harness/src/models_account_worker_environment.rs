// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Applying a selected account to a worker clears inherited selectors, retains
/// unrelated variables, and produces an unambiguous command on repeated application.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_worker_environment_model() -> Model {
    aterm_spec::ty_model! {
        AccountWorkerEnvironment {
            const Buggy = 0;
            var provider = 0; var own = 0; var carried = 0; var phase = 0;
            var login = 0; var clean = 1; var kept = 1; var writable = 1;
            action Provider when (phase == 0 && 3 > provider) { provider = provider + 1; }
            action Own when (phase == 0 && own == 0) { own = 1; }
            action Carried when (phase == 0 && 2 > carried) { carried = carried + 1; }
            action Apply when (phase == 0) {
                phase = 1;
                login = if own == 0 && Buggy == 1 {2}
                    else if own == 1 && Buggy == 2 {0}
                    else if own == 1 && carried == 2 && Buggy == 6 {2} else {own};
                clean = if carried > 0 && Buggy == 3 {0} else {1};
                kept = if Buggy == 4 {0} else {1};
                writable = if carried > 0 && Buggy == 5 {0} else {1};
            }
            invariant SelectedLogin: phase == 0 || login == own;
            invariant OverridesCleared: clean == 1;
            invariant UnrelatedRetained: kept == 1;
            invariant CommandWritable: writable == 1;
        }
    }
}
