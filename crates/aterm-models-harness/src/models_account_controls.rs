// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An account decision uses the rotation and login controls still published together.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_controls_model() -> Model {
    aterm_spec::ty_model! {
        AccountControls {
            const Buggy = 0;
            var phase = 0; var before_mode = 0; var before_logins = 0;
            var after_mode = 0; var after_logins = 0; var admitted = 0;
            action ModeBefore when (phase == 0 && before_mode <= 1) { before_mode = before_mode + 1; }
            action ModeAfter when (phase == 0 && after_mode <= 1) { after_mode = after_mode + 1; }
            action LoginsBefore when (phase == 0 && before_logins == 0) { before_logins = 1; }
            action LoginsAfter when (phase == 0 && after_logins == 0) { after_logins = 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                admitted = if Buggy == 3 {0} else {
                    if (before_mode == after_mode || Buggy == 1)
                        && (before_logins == after_logins || Buggy == 2) {1} else {0}
                };
            }
            invariant CurrentMode: admitted == 0 || before_mode == after_mode;
            invariant CurrentLogins: admitted == 0 || before_logins == after_logins;
            invariant CurrentProgress: phase <= 1 || after_mode > before_mode || before_mode > after_mode
                || after_logins > before_logins || before_logins > after_logins || admitted == 1;
        }
    }
}
