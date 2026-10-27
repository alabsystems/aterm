// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local credential precedence after roster conflicts have been captured. This does not
/// establish mailbox ownership, global freshness, or authorization to contact a provider.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_mailbox_selection_model() -> Model {
    aterm_spec::ty_model! {
        AccountMailboxSelection {
            const Buggy = 0;
            var case = 0; var phase = 0; var selected = 0;
            action AnotherCase when (phase == 0 && case <= 7) { case = case + 1; }
            action Lookup when (phase == 0) {
                phase = 1;
                selected = if case == 0 {if Buggy == 1 {2} else {0}}
                    else {if case == 1 || case == 6 {if Buggy == 2 {2} else {1}}
                    else {if case == 2 || case == 7 {if Buggy == 3 {0} else {2}}
                    else {if case == 3 || case == 8 {if Buggy == 4 {0} else {3}}
                    else {if case == 4 {if Buggy == 5 {2} else {4}}
                    else {if Buggy == 6 {2} else {0}}}}}};
            }
            invariant ContestedRefuses: phase == 0 || case > 0 || selected == 0;
            invariant ExactWins: phase == 0 || ((1 > case || case > 1) && (6 > case || case > 6)) || selected == 1;
            invariant UncontestedWorks: phase == 0 || ((2 > case || case > 2) && (7 > case || case > 7)) || selected == 2;
            invariant DefaultWorks: phase == 0 || ((3 > case || case > 3) && (8 > case || case > 8)) || selected == 3;
            invariant WildcardWorks: phase == 0 || 4 > case || case > 4 || selected == 4;
            invariant MissingRefuses: phase == 0 || 5 > case || case > 5 || selected == 0;
        }
    }
}
