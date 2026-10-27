// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Usage keeps its source mode, account policy and local store through admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn usage_source_model() -> Model {
    aterm_spec::ty_model! {
        UsageSource {
            const Buggy = 0;
            var phase = 0; var case = 1; var written = 0;
            var preserved = 1; var source = 1;
            action AnotherCase when (phase == 0 && case <= 17) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Record when (phase == 1) {
                phase = 2;
                written = if case <= 4 || case == 9 || case == 12 {
                    if (case == 1 && Buggy == 1) || (case == 2 && Buggy == 2)
                        || (case == 3 && Buggy == 3) || (case == 4 && Buggy == 4)
                        || (case == 9 && Buggy == 9) || (case == 12 && Buggy == 12) {0} else {1}
                } else {
                    if (case == 5 && Buggy == 5) || (case == 6 && Buggy == 6)
                        || (case == 7 && Buggy == 7) || (case == 8 && Buggy == 8)
                        || (case == 10 && Buggy == 10) || (case == 11 && Buggy == 11)
                        || (case == 13 && Buggy == 13) || (case == 14 && Buggy == 14)
                        || (case == 15 && Buggy == 15) || (case == 16 && Buggy == 16)
                        || (case == 17 && Buggy == 17) || (case == 18 && Buggy == 18) {1} else {0}
                };
                preserved = if case == 5 && Buggy == 19 {0} else {1};
                source = if case == 2 && Buggy == 20 {0} else {1};
            }
            invariant SourceAdmission: phase <= 1 || written == 0 || case <= 4 || case == 9 || case == 12;
            invariant DefaultProgress: phase <= 1 || (case > 4 && case <= 8) || (case > 9 && case <= 11) || case > 12 || written == 1;
            invariant RefusalPreserves: phase <= 1 || written == 1 || preserved == 1;
            invariant SourceUnchanged: source == 1;
        }
    }
}
