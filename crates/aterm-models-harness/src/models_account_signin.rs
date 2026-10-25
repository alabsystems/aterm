// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Admission reads for fixed local files before a sign-in starts. Completion must perform
/// its own transaction: this model grants no credential rights and does not claim that
/// a successful read locks the files or prevents them changing during authentication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_signin_admission_model() -> Model {
    aterm_spec::ty_model! {
        AccountSigninAdmission {
            const Buggy = 0;
            var phase = 0; var fresh = 0; var state_bad = 0;
            var bindings_bad = 0; var accepted = 0;
            action Fresh when (phase == 0) { phase = 1; fresh = 1; }
            action Known when (phase == 0) { phase = 1; }
            action StateUnread when (phase == 0) { phase = 1; state_bad = 1; }
            action BindingsUnread when (phase == 0) { phase = 1; bindings_bad = 1; }
            action StateLost when (phase == 0) { phase = 1; state_bad = 2; }
            action Read when (phase == 1) {
                phase = 2;
                accepted = if fresh == 1 && Buggy == 4 {0}
                    else if (state_bad == 0 || (state_bad == 1 && Buggy == 1)
                        || (state_bad == 2 && Buggy == 3))
                        && (bindings_bad == 0 || Buggy == 2) {1} else {0};
            }
            invariant StateIsReadable: phase <= 1 || state_bad == 0 || accepted == 0;
            invariant BindingsAreReadable: phase <= 1 || bindings_bad == 0 || accepted == 0;
            invariant HealthyIsAdmitted: phase <= 1 || state_bad > 0 || bindings_bad > 0 || accepted == 1;
        }
    }
}
