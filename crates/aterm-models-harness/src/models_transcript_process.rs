// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A transcript reader admits only a matching process and returns its exact environment.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn transcript_process_model() -> Model {
    aterm_spec::ty_model! {
        TranscriptProcess {
            const Buggy = 0;
            var phase = 0; var case = 1; var accepted = 0; var intact = 1;
            action AnotherCase when (phase == 0 && case <= 20) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Read when (phase == 1) {
                phase = 2;
                accepted = if case <= 2 || case == 15 || case == 16 || case == 19 || case == 20 {
                    if (case == 1 && Buggy == 1) || (case == 2 && Buggy == 2)
                        || (case == 15 && Buggy == 15) || (case == 16 && Buggy == 16)
                        || (case == 19 && Buggy == 19) || (case == 20 && Buggy == 20) {0} else {1}
                } else {
                    if (case == 3 && Buggy == 3) || (case == 4 && Buggy == 4)
                        || (case == 5 && Buggy == 5) || (case == 6 && Buggy == 6)
                        || (case == 7 && Buggy == 7) || (case == 8 && Buggy == 8)
                        || (case == 9 && Buggy == 9) || (case == 10 && Buggy == 10)
                        || (case == 11 && Buggy == 11) || (case == 12 && Buggy == 12)
                        || (case == 13 && Buggy == 13) || (case == 14 && Buggy == 14)
                        || (case == 17 && Buggy == 17) || (case == 18 && Buggy == 18)
                        || (case == 21 && Buggy == 21) {1} else {0}
                };
                intact = if Buggy == 22 {0} else {1};
            }
            invariant MatchingProcess: phase <= 1 || accepted == 0 || case <= 2 || case == 15 || case == 16 || case == 19 || case == 20;
            invariant MatchingProgress: phase <= 1 || (case > 2 && case <= 14) || case == 17 || case == 18 || case == 21 || accepted == 1;
            invariant CapturedEnvironment: intact == 1;
        }
    }
}
