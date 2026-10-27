// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Source precedence is resolved before implicit mailbox aliases are created. Explicit
/// email keys retain specificity; the model does not establish mailbox ownership.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_mailbox_precedence_model() -> Model {
    aterm_spec::ty_model! {
        AccountMailboxPrecedence {
            const Buggy = 0;
            var source = 0; var case = 0; var phase = 0; var selected = 0;
            action AnotherSource when (phase == 0 && source == 0) { source = 1; }
            action AnotherCase when (phase == 0 && case <= 1) { case = case + 1; }
            action Resolve when (phase == 0) {
                phase = 1;
                selected = if case == 0 {if Buggy == 1 {2} else {1}}
                    else {if case == 1 {if Buggy == 2 {1} else {3}}
                    else {if Buggy == 3 {1} else {4}}};
            }
            invariant WinningLabel: phase == 0 || case > 0 || selected == 1;
            invariant PrimaryExplicit: phase == 0 || case == 0 || case > 1 || selected == 3;
            invariant SecondaryExplicit: phase == 0 || 2 > case || selected == 4;
        }
    }
}
