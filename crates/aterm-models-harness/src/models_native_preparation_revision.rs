// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A new native decision wakes both local retry layers without bypassing holds.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_preparation_revision_model() -> Model {
    aterm_spec::ty_model! {
        NativePreparationRevision {
            const Buggy = 0;
            var revision = 1; var history = 1; var held = 1;
            var calls = 0; var attempted = 0; var clock_held = 0;
            action Replace when (revision == 1) {
                revision = 2; history = if Buggy == 1 {2} else {history};
            }
            action Person when (held > 0 && calls == 0) { held = 2; }
            action Bot when (held > 0 && calls == 0) { held = 3; }
            action Blocked when (held > 0) {
                calls = if Buggy == 2 {1} else {calls};
                history = if Buggy == 2 {2} else {history};
            }
            action Allow when (held > 0) { held = 0; }
            action Future when (held == 0 && revision == 2 && calls == 0) {
                clock_held = 1;
                calls = if Buggy == 5 {1} else {calls};
                history = if Buggy == 5 {2} else {history};
            }
            action Try when (held == 0 && revision == 2 && calls == 0) {
                attempted = 1; clock_held = 0;
                history = if Buggy == 3 {history} else {2};
                calls = if Buggy == 3 {calls} else {1};
            }
            action Repeat when (calls == 1) { calls = if Buggy == 4 {2} else {calls}; clock_held = 0; }
            invariant HeldDoesNotRun: held == 0 || calls == 0;
            invariant UnadmittedHistory: calls > 0 || history == 1;
            invariant ReplacementProgress: attempted == 0 || (history == 2 && calls == 1);
            invariant OneAttempt: calls <= 1;
            invariant FutureDoesNotRun: clock_held == 0 || calls == 0;
        }
    }
}
