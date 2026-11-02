// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Successful default sign-ins fence prior observations without inventing a binding.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_explicit_signin_model() -> Model {
    aterm_spec::ty_model! {
        AccountExplicitSignIn {
            const Buggy = 0;
            var binding = 0; var case = 0; var phase = 0;
            var accepted = 0; var fenced = 0; var refreshed = 0;
            var created = 0; var preserved = 1;
            action Binding when (phase == 0 && binding == 0) { binding = 1; }
            action Case when (phase == 0 && 4 > case) { case = case + 1; }
            action Complete when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 {0} else if Buggy == 2 || 3 > case {1} else {0};
                fenced = if Buggy == 3 {0} else if 3 > case {1} else {0};
                refreshed = if Buggy == 5 {0} else if 3 > case && binding == 1 {1} else {0};
                created = if Buggy == 4 || (case == 3 && binding == 0) {1} else {0};
                preserved = if Buggy == 6 {0} else if case == 4 {1} else {0};
            }
            invariant CurrentAccepted: phase == 0 || case > 2 || accepted == 1;
            invariant RejectedClosed: phase == 0 || 3 > case || accepted == 0;
            invariant CurrentReceipt: phase == 0 || case > 2 || fenced == 1;
            invariant RefreshExisting: phase == 0 || case > 2 || binding == 0 || refreshed == 1;
            invariant NoBindingInvented: accepted == 0 || created == 0;
            invariant StalePreserved: phase == 0 || 4 > case || preserved == 1;
        }
    }
}
