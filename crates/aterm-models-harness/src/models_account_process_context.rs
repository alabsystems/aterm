// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Account process identity binds launch and credential facts, not live record metadata.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_process_context_model() -> Model {
    aterm_spec::ty_model! {
        AccountProcessContext {
            const Buggy = 0;
            var phase = 0; var field = 1;
            var same = 1; var reverse = 1; var reflexive = 1;
            action AnotherField when (phase == 0 && field <= 22) { field = field + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Compare when (phase == 1) {
                phase = 2;
                same = if (Buggy == 1 && field == 1) || (Buggy == 2 && field == 2)
                    || (Buggy == 3 && field == 3) || (Buggy == 4 && field == 4)
                    || (Buggy == 5 && field == 5) || (Buggy == 6 && field == 6)
                    || (Buggy == 7 && field == 7) || (Buggy == 8 && field == 8)
                    || (Buggy == 9 && field == 9) || (Buggy == 10 && field == 10)
                    || (Buggy == 11 && field == 11) || (Buggy == 12 && field == 12)
                    || (Buggy == 13 && field == 13) || (Buggy == 14 && field == 14)
                    || (Buggy == 15 && field == 15) || (Buggy == 16 && field == 16)
                    || (Buggy == 17 && field == 17) || (Buggy == 18 && field == 18)
                    || (Buggy == 19 && field == 19) || (Buggy == 20 && field == 20)
                    || (Buggy == 21 && field == 21) || (Buggy == 22 && field == 22)
                    || (Buggy == 23 && field == 23) {
                    if field == 1 || field > 18 {0} else {1}
                } else {if field == 1 || field > 18 {1} else {0}};
                reverse = if Buggy == 25 {
                    if field == 1 || field > 18 {0} else {1}
                } else {if field == 1 || field > 18 {1} else {0}};
                reflexive = if Buggy == 24 {0} else {1};
            }
            invariant ChangedContextRejected: phase <= 1 || field == 1 || field > 18 || same == 0;
            invariant MetadataAndIdentityKept: phase <= 1 || (field > 1 && field <= 18) || same == 1;
            invariant Reflexive: reflexive == 1;
            invariant Symmetric: phase <= 1 || same == reverse;
        }
    }
}
