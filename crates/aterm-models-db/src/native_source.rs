// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Native source admission refuses stale or excluded sources before a local
/// receipt callback, retains custody through it, and releases custody on error.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_source_model() -> Model {
    aterm_spec::ty_model! {
        DbNativeSource {
            const Buggy = 0;
            var scenario = 0; var done = 0; var called = 0;
            var accepted = 0; var held = 1; var released = 1;
            action Scenario when (done == 0 && 9 > scenario) { scenario = scenario + 1; }
            action Admit when (done == 0) {
                done = 1;
                called = if scenario == 0 || scenario == 8 ||
                    (Buggy == 1 && scenario == 2) || (Buggy == 2 && scenario == 5) {1} else {0};
                accepted = if scenario == 8 {if Buggy == 4 {1} else {0}} else {
                    if scenario == 0 {if Buggy == 6 {0} else {1}} else {
                        if (Buggy == 1 && scenario == 2) || (Buggy == 2 && scenario == 5) {1} else {0}
                    }
                };
                held = if Buggy == 3 && scenario == 0 {0} else {1};
                released = if Buggy == 5 && scenario == 8 {0} else {1};
            }
            invariant RefuseChangedOrExcluded: scenario == 0 || scenario == 8 || called == 0;
            invariant HoldThroughReceipt: called == 0 || held == 1;
            invariant ReleaseAfterReceipt: released == 1;
            invariant AvailableSource: done == 0 || scenario > 0 || accepted == 1;
            invariant PropagateCallbackFailure: 8 > scenario || scenario > 8 || accepted == 0;
            bound ScenarioDomain: 0 <= scenario && scenario <= 9;
        }
    }
}
