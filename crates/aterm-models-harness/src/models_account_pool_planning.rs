// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Observation and command preparation do not allocate a rotation; consuming a
/// plan checks current policy, credentials and time before exposing its command.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_planning_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolPlanning {
            const Buggy = 0;
            var case = 0; var phase = 0; var finished = 0;
            var committed = 0; var count = 0; var active = 0; var canary = 0;
            var evidence = 0; var line = 0; var external = 0;
            action AnotherCase when (phase == 0 && case <= 11) { case = case + 1; }
            action Plan when (phase == 0) {
                phase = 1; evidence = 1;
                count = if Buggy == 1 {1} else {0};
            }
            action Change when (phase == 1) {
                phase = 2;
                external = if case == 6 {1} else {0};
                count = if case == 6 {1} else {0};
                active = if case == 6 {1} else {0};
                canary = if case == 6 {1} else {0};
            }
            action Finish when (phase == 2) {
                phase = 3; finished = 1;
                committed = if Buggy == 9 || (Buggy == 15 && case == 10)
                    || (Buggy == 16 && case == 11) || (Buggy == 17 && case == 12) {0}
                    else if (Buggy == 2 && case == 1) || (Buggy == 3 && case == 2)
                        || (Buggy == 4 && case == 3) || (Buggy == 5 && case == 4)
                        || (Buggy == 6 && case == 5) || (Buggy == 7 && case == 6)
                        || (Buggy == 8 && case == 7) || (Buggy == 14 && case == 9) {1}
                    else if case == 0 || case == 8 || case > 9 {1} else {0};
                count = external + if Buggy == 9 || (Buggy == 15 && case == 10)
                    || (Buggy == 16 && case == 11) || (Buggy == 17 && case == 12) {0}
                    else if (Buggy == 2 && case == 1) || (Buggy == 3 && case == 2)
                        || (Buggy == 4 && case == 3) || (Buggy == 5 && case == 4)
                        || (Buggy == 6 && case == 5) || (Buggy == 7 && case == 6)
                        || (Buggy == 8 && case == 7) || (Buggy == 14 && case == 9) {1}
                    else if case == 0 || case == 8 || case > 9 {1} else {0};
                active = if Buggy == 11 {0} else if external == 1 {1}
                    else if Buggy == 9 || (Buggy == 15 && case == 10)
                        || (Buggy == 16 && case == 11) || (Buggy == 17 && case == 12) {0}
                    else if (Buggy == 2 && case == 1) || (Buggy == 3 && case == 2)
                        || (Buggy == 4 && case == 3) || (Buggy == 5 && case == 4)
                        || (Buggy == 6 && case == 5) || (Buggy == 7 && case == 6)
                        || (Buggy == 8 && case == 7) || (Buggy == 14 && case == 9) {1}
                    else if case == 0 || case == 8 || case > 9 {1} else {0};
                canary = if Buggy == 12 {0} else if external == 1 {1}
                    else if Buggy == 9 || (Buggy == 15 && case == 10)
                        || (Buggy == 16 && case == 11) || (Buggy == 17 && case == 12) {0}
                    else if (Buggy == 2 && case == 1) || (Buggy == 3 && case == 2)
                        || (Buggy == 4 && case == 3) || (Buggy == 5 && case == 4)
                        || (Buggy == 6 && case == 5) || (Buggy == 7 && case == 6)
                        || (Buggy == 8 && case == 7) || (Buggy == 14 && case == 9) {1}
                    else if case == 0 || case == 8 || case > 9 {1} else {0};
                line = if Buggy == 8 {1} else if Buggy == 9
                    || (Buggy == 15 && case == 10) || (Buggy == 16 && case == 11)
                    || (Buggy == 17 && case == 12) {0}
                    else if (Buggy == 2 && case == 1) || (Buggy == 3 && case == 2)
                        || (Buggy == 4 && case == 3) || (Buggy == 5 && case == 4)
                        || (Buggy == 6 && case == 5) || (Buggy == 7 && case == 6)
                        || (Buggy == 8 && case == 7) || (Buggy == 14 && case == 9) {1}
                    else if case == 0 || case == 8 || case > 9 {1} else {0};
                evidence = if Buggy == 10 {0} else {1};
            }
            action Drop when (phase == 1 || phase == 2) {
                phase = 3;
                committed = if Buggy == 13 {1} else {0};
                count = external + if Buggy == 13 {1} else {0};
                active = if external == 1 || Buggy == 13 {1} else {0};
                canary = if external == 1 || Buggy == 13 {1} else {0};
            }
            invariant Planning: phase == 0 || phase > 1 || (count == 0 && active == 0 && canary == 0);
            invariant Allocations: count == external + committed;
            invariant Admission: committed == 0 || case == 0 || case == 8 || case > 9;
            invariant Active: active == if count > 0 {1} else {0};
            invariant Canary: canary == if count > 0 {1} else {0};
            invariant Command: line == committed;
            invariant Current: phase <= 2 || finished == 0 || (case > 0 && case <= 7) || case == 9 || committed == 1;
            invariant Evidence: phase == 0 || evidence == 1;
            invariant Dropped: finished == 1 || committed == 0;
        }
    }
}
