// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Deferred account work stays with its originating namespace and survives refused writes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_origin_model() -> Model {
    aterm_spec::ty_model! {
        AccountOrigin {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var refused = 0; var pending = 0; var changed = 0; var foreign = 0;
            var notified = 0; var left = 0; var correct = 0; var restarts = 0; var final_notified = 0;
            action AnotherCase when (phase == 0 && case <= 23) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                refused = if Buggy == 7 && case == 15 {1}
                    else {if (case > 2 && case <= 14) || case > 17 {1} else {0}};
                pending = if Buggy == 3 && case == 5 {0}
                    else {if (case > 2 && case <= 14) || case > 17 {1} else {0}};
                changed = if Buggy == 1 && case == 3 {1}
                    else {if case <= 2 || (case > 14 && case <= 17) {1} else {0}};
                foreign = if Buggy == 2 && case == 8 {1} else {0};
                notified = if Buggy == 6 && case == 3 {1} else {if case == 1 || case == 15 {1} else {0}};
                left = if Buggy == 4 && case == 1 {1} else {0};
                correct = if Buggy == 8 && case == 17 {0} else {1};
                restarts = if Buggy == 5 && case == 10 {2} else {1};
                final_notified = if case == 1 || (case > 2 && case <= 6) || case == 15 {1} else {0};
            }
            invariant RefusalPreservesOrigin: phase <= 1 || refused == 0 || changed == 0;
            invariant NoForeignWrites: foreign == 0;
            invariant PendingRetained: phase <= 1 || refused == 0 || pending == 1;
            invariant RetryCompletes: phase <= 1 || (left == 0 && correct == 1);
            invariant OneRestart: phase <= 1 || restarts == 1;
            invariant NoPrematureNotification: phase <= 1 || refused == 0 || notified == 0;
            invariant ClockAndWaitAreNotIdentity: phase <= 1 || case <= 14 || case > 17 || refused == 0;
        }
    }
}
