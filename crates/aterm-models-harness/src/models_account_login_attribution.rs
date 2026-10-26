// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An unresolved login file cannot acquire the default account's identity.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_login_attribution_model() -> Model {
    aterm_spec::ty_model! {
        AccountLoginAttribution {
            const Buggy = 0;
            var provider = 0; var case = 0; var phase = 0;
            var judged = 0; var stored = 0; var retained = 1;
            action AnotherProvider when (phase == 0 && provider == 0) { provider = 1; }
            action AnotherCase when (phase == 0 && 7 > case) { case = case + 1; }
            action Observe when (phase == 0) {
                phase = 1;
                judged = if 2 > case {if case == 0 && Buggy == 3 {0} else {1}}
                    else if case == 2 {if Buggy == 2 {0} else {2}}
                    else if case == 3 && Buggy == 1 {1}
                    else if case == 7 && Buggy == 6 {2} else {0};
                stored = if 3 > case || case == 5 {
                    if case == 5 && Buggy == 7 {0} else {1}
                } else {if case == 4 && Buggy == 4 {1} else {0}};
                retained = if case == 3 && Buggy == 5 {0} else {1};
            }
            invariant ExactAttribution: phase == 0
                || (2 > case && judged == 1) || (case == 2 && judged == 2)
                || (case > 2 && judged == 0);
            invariant UnknownRefused: phase == 0 || 3 > case || case == 5 || stored == 0;
            invariant KnownStored: phase == 0 || case == 3 || case == 4 || case == 6 || case == 7 || stored == 1;
            invariant UnboundPathRetained: phase == 0 || case == 0 || case == 1 || case == 2
                || case == 5 || case == 7 || retained == 1;
        }
    }
}
