// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two renewal keys consume their own changed input only after local admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn mailbox_renewal_admission_model() -> Model {
    aterm_spec::ty_model! {
        MailboxRenewalAdmission {
            const Buggy = 0;
            var hold = 0;
            var left = 1; var right = 1;
            var left_stamp = 1; var right_stamp = 1;
            var left_calls = 0; var right_calls = 0;
            action Busy when (hold == 0 && left_calls == 0 && right_calls == 0) { hold = 1; }
            action Person when (hold == 0 && left_calls == 0 && right_calls == 0) { hold = 2; }
            action Bot when (hold == 0 && left_calls == 0 && right_calls == 0) { hold = 3; }
            action Release when (hold > 0) { hold = 0; }
            action Left {
                left = if (hold == 0 && (left == 1 || Buggy == 4)) || (hold > 0 && Buggy == 2) {2} else {left};
                left_stamp = if (hold == 0 && (left == 1 || Buggy == 4)) || (hold > 0 && Buggy == 2) {2}
                    else if hold > 0 && Buggy == 1 {0} else {left_stamp};
                right_stamp = if hold == 0 && left == 1 && Buggy == 3 {0} else {right_stamp};
                left_calls = if (hold == 0 && (left == 1 || Buggy == 4)) || (hold > 0 && Buggy == 2) {left_calls + 1} else {left_calls};
            }
            action Right {
                right = if (hold == 0 && (right == 1 || Buggy == 4)) || (hold > 0 && Buggy == 2) {3} else {right};
                right_stamp = if (hold == 0 && (right == 1 || Buggy == 4)) || (hold > 0 && Buggy == 2) {3}
                    else if hold > 0 && Buggy == 1 {0} else {right_stamp};
                left_stamp = if hold == 0 && right == 1 && Buggy == 3 {0} else {left_stamp};
                right_calls = if (hold == 0 && (right == 1 || Buggy == 4)) || (hold > 0 && Buggy == 2) {right_calls + 1} else {right_calls};
            }
            invariant HeldDoesNotAdmit: hold == 0 || left_calls + right_calls == 0;
            invariant LeftHistory: (left == 1 && left_stamp == 1 && left_calls == 0)
                || (left == 2 && left_stamp == 2 && left_calls == 1);
            invariant RightHistory: (right == 1 && right_stamp == 1 && right_calls == 0)
                || (right == 3 && right_stamp == 3 && right_calls == 1);
        }
    }
}
