// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Failed sign-in observations recheck their row and parent at publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_attempt_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountAttemptPolicy {
            const Buggy = 0;
            var row = 0; var parent = 0; var readable = 1;
            var phase = 0; var okay = 0; var observed = 0; var written = 0;

            action RowA when (2 > phase && row == 1) {
                row = 0; observed = 0;
            }
            action RowB when (2 > phase && row == 0) {
                row = 1; observed = 0;
            }
            action Replace when (2 > phase && parent == 0) {
                parent = 1; observed = 0;
            }
            action Unreadable when (2 > phase && readable == 1) {
                readable = 0; observed = 0;
            }
            action Readable when (2 > phase && readable == 0) {
                readable = 1; observed = 0;
            }
            action Assess when (phase == 0) {
                phase = 1; observed = 1;
                okay = if Buggy == 6 {0} else if Buggy == 5 || ((row == 0 || Buggy == 1) &&
                    (parent == 0 || Buggy == 2) && (readable == 1 || Buggy == 3)) {1} else {0};
            }
            action Record when (phase == 1) {
                phase = 2; observed = 0;
                written = if Buggy == 7 {0} else if okay == 1 && (Buggy == 4 ||
                    ((row == 0 || Buggy == 1) && (parent == 0 || Buggy == 2) &&
                    (readable == 1 || Buggy == 3))) {1} else {0};
            }

            invariant CurrentRow: written == 0 || row == 0;
            invariant CurrentParent: written == 0 || parent == 0;
            invariant ReadablePolicy: written == 0 || readable == 1;
            invariant AssessmentCurrent: observed == 0 || okay == 0 ||
                (row == 0 && parent == 0 && readable == 1);
            invariant AvailableAssessment: observed == 0 || row > 0 || parent > 0 ||
                1 > readable || okay == 1;
            invariant AvailablePublication: phase <= 1 || okay == 0 || row > 0 || parent > 0 ||
                1 > readable || written == 1;
        }
    }
}
