// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Sign-in admission and completion follow a captured positive account generation.
/// Browser cleanup is independent of whether its result remains admissible.
/// A recorded result follows command start and retains a later observed time.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_signin_epoch_model() -> Model {
    aterm_spec::ty_model! {
        AccountSignInEpoch {
            const Buggy = 0;
            var phase = 0; var browser = 0; var provider = 1; var case = 0;
            var opened = 0; var recorded = 0; var tidied = 0; var event_order = 1;
            action Browser when (phase == 0 && browser == 0) { browser = 1; }
            action AnotherProvider when (phase == 0 && browser == 1 && 4 > provider) { provider = provider + 1; }
            action AnotherCase when (phase == 0 && 11 > case) { case = case + 1; }
            action Run when (phase == 0) {
                phase = 1;
                opened = if case > 0 && 4 > case {
                    if Buggy == 1 {1} else {0}
                } else { if Buggy == 2 {0} else {1} };
                recorded = if case > 0 && 7 > case {
                    if Buggy == 3 {1} else {0}
                } else { if Buggy == 4 {0} else {1} };
                tidied = if browser == 1 && (case == 0 || case > 3 || Buggy == 1) {
                    if Buggy == 5 || Buggy == 2 {0} else {1}
                } else {0};
                event_order = if case > 0 && 7 > case {1} else {
                    if case == 11 { if Buggy == 6 {0} else {1} }
                    else { if Buggy == 7 {1} else {2} }
                };
            }
            invariant StaleBeforeOpenRefused: phase == 0 || case == 0 || case > 3 || opened == 0;
            invariant CurrentBeforeOpenAdmitted: phase == 0 || (case > 0 && 4 > case) || opened == 1;
            invariant StaleResultNotRecorded: phase == 0 || case == 0 || case > 6 || recorded == 0;
            invariant CurrentResultRecorded: phase == 0 || (case > 0 && 7 > case) || recorded == 1;
            invariant OpenedBrowserTidied: phase == 0 || browser == 0 || opened == 0 || tidied == 1;
            invariant ResultNotBeforeStart: phase == 0 || recorded == 0 || event_order > 0;
            invariant LaterObservationKept: phase == 0 || recorded == 0 || case == 11 || event_order == 2;
        }
    }
}
