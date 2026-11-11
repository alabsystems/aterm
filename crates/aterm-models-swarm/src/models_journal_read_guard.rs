// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An existing journal lock can exclude writers for a read without repairing
/// or initializing storage. Record authentication is a separate obligation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn journal_read_guard_model() -> Model {
    aterm_spec::ty_model! {
        JournalReadGuard {
            const Buggy = 0;
            var scenario = 0; var done = 0; var admitted = 0;
            var held = 1; var unchanged = 1; var released = 1;
            action Case when (done == 0 && 11 > scenario) { scenario = scenario + 1; }
            action Take when (done == 0) {
                done = 1;
                admitted = if Buggy == 5 {0} else {
                    if scenario == 0 || scenario == 7 || scenario == 8 || scenario == 9
                        || (Buggy == 1 && scenario == 2) || (Buggy == 2 && scenario == 3) {1} else {0}
                };
                held = if (Buggy == 1 && (scenario == 0 || scenario == 2 || scenario == 7
                    || scenario == 8 || scenario == 9)) || (Buggy == 2 && scenario == 3) {0} else {1};
                unchanged = if Buggy == 3 {0} else {1};
                released = if Buggy == 4 {0} else {1};
            }
            invariant ExistingPrivateCustody: scenario == 0 || scenario == 7 || scenario == 8
                || scenario == 9 || admitted == 0;
            invariant WriterExcluded: admitted == 0 || held == 1;
            invariant NoSourceMutation: unchanged == 1;
            invariant Released: released == 1;
            invariant Available: done == 0 || (scenario > 0 && 7 > scenario) || scenario > 9 || admitted == 1;
            bound Cases: 0 <= scenario && scenario <= 11;
        }
    }
}
