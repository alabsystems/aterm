// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two mature notices, interrupted batches and a later retry. Time is a
/// monotonic input; an attempted refusal costs the same cooldown as success.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recovery_cooldown_model() -> Model {
    aterm_spec::ty_model! {
        DbRecoveryAttemptCooldown {
            const Buggy = 0;
            var phase = 0; var now = 60; var a = 60; var b = 60;
            var calls = 0; var issued = 0; var first = 0;
            var stopped = 0; var error = 0; var ready = 2;
            action Stop when (phase == 0) {
                phase = 1; stopped = 1;
                a = if Buggy == 1 {120} else {60};
                b = if Buggy == 1 {120} else {60};
                ready = if Buggy == 1 {0} else {2};
            }
            action Deadline when (phase == 0) {
                phase = 2; error = 1;
                a = if Buggy == 1 {120} else {60};
                b = if Buggy == 1 {120} else {60};
                ready = if Buggy == 1 {0} else {2};
            }
            action Partial when (phase == 0) {
                phase = 3; a = 120; b = if Buggy == 1 {120} else {60};
                calls = 1; issued = 1; first = 1; stopped = 1;
                ready = if Buggy == 1 {0} else {1};
            }
            action Refused when (phase == 0) {
                phase = 4; a = if Buggy == 2 {60} else {120};
                calls = 1; first = 1; stopped = 1;
                ready = if Buggy == 2 {2} else {1};
            }
            action Delayed when (phase == 0) {
                phase = 5; now = 90; a = if Buggy == 3 {120} else {150};
                calls = 1; first = 1; stopped = 1; ready = 1;
            }
            action Complete when (phase == 0) {
                phase = 6; a = 120; b = 120; calls = 2;
                issued = if Buggy == 7 {3} else {2}; first = 1; ready = 0;
            }
            action Resume when (phase > 0 && phase <= 5) {
                phase = 7; now = now + 1;
                a = if a == 60 {now + 61} else {a}; b = now + 61;
                calls = if a == 60 {2} else {1}; issued = if a == 60 {2} else {1};
                first = if a == 60 || Buggy == 4 {1} else {2};
                stopped = 0; error = 0; ready = 0;
            }
            action LateResume when (phase > 2 && phase <= 5) {
                phase = 10; now = 150; a = 210; b = 210;
                calls = 2; issued = 2; first = if Buggy == 4 {1} else {2};
                stopped = 0; error = 0; ready = 0;
            }
            action Rescan when (phase == 6) {
                phase = 8; now = 61;
                calls = if Buggy == 5 {2} else {0}; issued = if Buggy == 5 {2} else {0};
                first = if Buggy == 5 {1} else {0};
            }
            action Boundary when (phase == 8) {
                phase = 9; now = 120; a = 180; b = 180;
                calls = if Buggy == 6 {0} else {2}; issued = if Buggy == 6 {0} else {2};
                first = 1;
            }
            invariant UntouchedStayReady: phase > 2 || (a == 60 && b == 60 && ready == 2);
            invariant PartialChargesOnlyAttempt: 3 > phase || phase > 3 || (a == 120 && b == 60 && ready == 1);
            invariant RefusalStillCoolsDown: 4 > phase || phase > 4 || (a == 120 && b == 60 && ready == 1 && issued == 0);
            invariant ActualStartTime: 5 > phase || phase > 5 || (a == now + 60 && b == 60 && ready == 1);
            invariant OldestResumesFirst: 7 > phase || phase > 7 || first == if calls == 2 {1} else {2};
            invariant NoEarlyRetry: 8 > phase || phase > 8 || (calls == 0 && issued == 0);
            invariant ExactBoundaryProgress: 9 > phase || phase > 9 || (calls == 2 && issued == 2);
            invariant LateOldestFirst: 10 > phase || phase > 10 || first == 2;
            invariant CountOnlySuccess: issued <= calls;
        }
    }
}
