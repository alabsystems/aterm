// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Snapshot and fallback reads publish only into their captured context.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_snapshot_read_model() -> Model {
    aterm_spec::ty_model! {
        HarnessSnapshotRead {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var reads = 0; var follows = 0; var published = 0; var returned = 0;
            action AnotherCase when (phase == 0 && case <= 20) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Observe when (phase == 1) {
                phase = 2;
                reads = if Buggy == 7 && case == 15 {1}
                    else {if case == 15 || case == 19 || case == 20 {0} else {1}};
                follows = if (Buggy == 4 && case == 11) || (Buggy == 15 && case == 18) || (Buggy == 21 && case == 12) {1}
                    else {if (case > 7 && case <= 10) || case == 13 || case == 14 || case == 17 || case > 18 {1} else {0}};
                published = if (Buggy == 1 && case == 2) || (Buggy == 2 && case == 3)
                    || (Buggy == 3 && case == 7) || (Buggy == 5 && case == 13)
                    || (Buggy == 6 && case == 14) || (Buggy == 8 && case == 16)
                    || (Buggy == 9 && case == 17) || (Buggy == 14 && case == 10)
                    || (Buggy == 18 && case == 20) || (Buggy == 19 && case == 4)
                    || (Buggy == 20 && case == 5) || (Buggy == 22 && case == 21) {1}
                    else {if (Buggy == 10 && case == 1) || (Buggy == 11 && case == 6)
                    || (Buggy == 12 && case == 8) || (Buggy == 13 && case == 9) {0}
                    else {if case == 1 || case == 6 || case == 8 || case == 9 || case == 19 {1} else {0}}};
                returned = if Buggy == 16 && case == 8 {1}
                    else {if Buggy == 17 && case == 19 {0}
                    else {if case == 1 || case == 6 || case == 19 {1} else {0}}};
            }
            invariant ReadAdmission: phase <= 1 || reads == if case == 15 || case == 19 || case == 20 {0} else {1};
            invariant FallbackAdmission: phase <= 1 || follows ==
                if (case > 7 && case <= 10) || case == 13 || case == 14 || case == 17 || case > 18 {1} else {0};
            invariant Publication: phase <= 1 || published ==
                if case == 1 || case == 6 || case == 8 || case == 9 || case == 19 {1} else {0};
            invariant ReadResult: phase <= 1 || returned == if case == 1 || case == 6 || case == 19 {1} else {0};
        }
    }
}
