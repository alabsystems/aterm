// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One captured process observation and its recording attempt. The native transition
/// changes bindings or the raw roster before acquiring the recording lock. This does not
/// prove provider identity, token contents, or exclusion of noncooperating filesystem edits.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_login_observation_model() -> Model {
    aterm_spec::ty_model! {
        AccountLoginObservation {
            const Buggy = 0;
            var phase = 0; var case = 1; var written = 0;
            action AnotherCase when (phase == 0 && case <= 10) {
                case = case + 1;
            }
            action Choose when (phase == 0) { phase = 1; }
            action Record when (phase == 1) {
                phase = 2;
                written = if case == 1 || case == 11 { if Buggy == 10 {0} else {1} }
                    else if (case == 2 && Buggy == 1) || (case == 3 && Buggy == 2)
                        || (case == 4 && Buggy == 3) || (case == 5 && Buggy == 4)
                        || (case == 6 && Buggy == 5) || (case == 7 && Buggy == 6)
                        || (case == 8 && Buggy == 7) || (case == 9 && Buggy == 8)
                        || (case == 10 && Buggy == 9) {1} else {0};
            }
            invariant OnlyCurrentRecords: phase <= 1 || written == 0 || case == 1 || case == 11;
            invariant CurrentRecords: phase <= 1 || (case > 1 && case <= 10) || written == 1;
        }
    }
}
