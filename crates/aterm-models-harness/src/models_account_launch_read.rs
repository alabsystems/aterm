// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Launch attribution after one recoverable read, with unknown records treated
/// conservatively. The native fixture supplies journal and identity failures.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_launch_read_model() -> Model {
    aterm_spec::ty_model! {
        AccountLaunchRead {
            const Buggy = 0;
            var phase = 0; var shape = 0; var fault = 0; var on = 0; var exe = 0;
            var several = 0; var armed = 0; var matching_login = 1;
            action AnotherShape when (phase == 0 && 2 > shape) { shape = shape + 1; }
            action AnotherFault when (phase == 0 && 6 > fault) { fault = fault + 1; }
            action EnableTap when (phase == 0 && on == 0) { on = 1; }
            action HaveExecutable when (phase == 0 && exe == 0) { exe = 1; }
            action Read when (phase == 0) {
                phase = 1;
                several = if Buggy == 1 && fault == 1 {
                    if shape == 2 {0} else {1}
                } else {
                    if fault > 1 { if Buggy == 2 {0} else {1} }
                    else { if shape == 2 {1} else {0} }
                };
                armed = if Buggy == 5 {0} else {
                    if on == 1 && exe == 1 && (2 > fault || Buggy == 3) {1} else {0}
                };
                matching_login = if Buggy == 4 && on == 1 && exe == 1 && 2 > fault {0} else {1};
            }
            invariant RecoveredCardinality:
                phase == 0 || fault > 1 || (shape == 2 && several == 1) || (2 > shape && several == 0);
            invariant UnknownEvidenceRestricted:
                phase == 0 || 2 > fault || several == 1;
            invariant TapOnlyWithKnownEnabledImage:
                phase == 0 || armed == 0 || (2 > fault && on == 1 && exe == 1);
            invariant KnownEnabledTapAvailable:
                phase == 0 || fault > 1 || on == 0 || exe == 0 || armed == 1;
            invariant LoginComesFromSnapshot: matching_login == 1;
        }
    }
}
