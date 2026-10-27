// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Explicit email credentials do not acquire other email spellings or machine labels.
/// This bounded local configuration model does not establish mailbox ownership.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_mailbox_scope_model() -> Model {
    aterm_spec::ty_model! {
        AccountMailboxScope {
            const Buggy = 0;
            var source = 0; var phase = 0;
            var own = 0; var spelling = 0; var machine = 0;
            action AnotherSource when (phase == 0 && source <= 5) { source = source + 1; }
            action Resolve when (phase == 0) {
                phase = 1;
                own = if Buggy == 1 {0} else {1};
                spelling = if Buggy == 2 {1} else {0};
                machine = if Buggy == 3 {1} else {0};
            }
            invariant OwnerWorks: phase == 0 || own == 1;
            invariant ExactSpelling: phase == 0 || spelling == 0;
            invariant NoMachineWidening: phase == 0 || machine == 0;
        }
    }
}
