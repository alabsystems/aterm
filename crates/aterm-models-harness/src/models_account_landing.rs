// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Delayed account completion retains work until its durable write succeeds.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_landing_model() -> Model {
    aterm_spec::ty_model! {
        AccountLanding {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var pending = 1; var moved = 0; var recorded = 0; var notified = 0;
            action AnotherCase when (phase == 0 && case <= 10) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                pending = if (Buggy == 1 && case == 1) || (Buggy == 5 && case == 5) || (Buggy == 6 && case == 6) {1}
                    else {if (Buggy == 3 && case == 3) || (Buggy == 9 && case == 9)
                        || case == 1 || case == 2 || case == 5 || case == 6 || case == 8 || case == 10 {0} else {1}};
                moved = if (Buggy == 2 && case == 2) || (Buggy == 4 && case == 4)
                    || case == 1 || case == 5 || case == 6 || case == 8 {1} else {0};
                notified = if Buggy == 7 && case == 7 {1} else {if Buggy == 8 && case == 8 {2}
                    else {if case == 1 || case == 5 || case == 6 || case == 8 {1} else {0}}};
                recorded = if (Buggy == 10 && case == 10) || (Buggy == 11 && case == 11) {1}
                    else {if Buggy == 12 && case == 1 {0}
                        else {if case == 1 || case == 2 || case == 5 || case == 6 || case == 8 {1} else {0}}};
            }
            invariant PublishedAfterCommit: phase <= 1 || moved == 0 || recorded == 1;
            invariant NotifiedAfterCommit: phase <= 1 || notified == 0 || (recorded == 1 && moved == 1);
            invariant RetryRetained: phase <= 1 || case <= 2 || case == 5 || case == 6 || case == 8 || case == 10 || pending == 1;
            invariant SuccessCompletes: phase <= 1 || case == 2 || case == 3 || case == 4 || case == 7 || case > 8 || (pending == 0 && moved == 1 && recorded == 1);
            invariant KnownFailure: phase <= 1 || case <= 1 || case > 2 || (pending == 0 && moved == 0 && recorded == 1 && notified == 0);
            invariant CanceledIsQuiet: phase <= 1 || case <= 9 || (recorded == 0 && moved == 0 && notified == 0);
            invariant AtMostOnce: notified <= 1;
        }
    }
}
