// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A restart carries the exact source, target row, reason and current generation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_restart_source_model() -> Model {
    aterm_spec::ty_model! {
        AccountRestartSource {
            const Buggy = 0;
            var phase = 0; var case = 1; var admitted = 0;
            action AnotherCase when (phase == 0 && case <= 28) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                admitted = if (Buggy == 1 && case == 1) || (Buggy == 2 && case == 2)
                    || (Buggy == 3 && case == 3) || (Buggy == 4 && case == 4)
                    || (Buggy == 5 && case == 5) || (Buggy == 6 && case == 6)
                    || (Buggy == 7 && case == 7) || (Buggy == 8 && case == 8)
                    || (Buggy == 9 && case == 9) || (Buggy == 10 && case == 10)
                    || (Buggy == 11 && case == 11) || (Buggy == 12 && case == 12)
                    || (Buggy == 13 && case == 13) || (Buggy == 14 && case == 14)
                    || (Buggy == 15 && case == 15) || (Buggy == 16 && case == 16)
                    || (Buggy == 17 && case == 17) || (Buggy == 18 && case == 18)
                    || (Buggy == 19 && case == 19) || (Buggy == 20 && case == 20)
                    || (Buggy == 21 && case == 21) || (Buggy == 22 && case == 22)
                    || (Buggy == 23 && case == 23) || (Buggy == 24 && case == 24)
                    || (Buggy == 25 && case == 25) || (Buggy == 26 && case == 26)
                    || (Buggy == 27 && case == 27) || (Buggy == 28 && case == 28)
                    || (Buggy == 29 && case == 29) {
                        if case == 1 || case == 23 || case == 24 || case == 29 {0} else {1}
                    } else {if case == 1 || case == 23 || case == 24 || case == 29 {1} else {0}};
            }
            invariant SelectedContext: phase <= 1 || case == 1 || case == 23 || case == 24 || case > 26 || admitted == 0;
            invariant Progress: phase <= 1 || (case > 1 && case <= 22) || case > 24 || admitted == 1;
            invariant RequiredSelection: phase <= 1 || case <= 26 || admitted == if case == 29 {1} else {0};
        }
    }
}
