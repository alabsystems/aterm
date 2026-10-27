// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A return consumes its receipt and publishes only a current worker's requested login.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_return_model() -> Model {
    aterm_spec::ty_model! {
        AccountReturn {
            const Buggy = 0;
            var phase = 0; var case = 1;
            var accepted = 0; var intact = 1; var consumed = 0;
            var returned = 0; var marker = 0; var owed = 0; var observed = 0;
            action AnotherCase when (phase == 0 && case <= 14) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Commit when (phase == 1) {
                phase = 2;
                accepted = if Buggy == 1 && case == 5 {1}
                    else {if case == 1 || (case > 8 && case <= 11) || case == 15 {1} else {0}};
                intact = if Buggy == 2 && case == 2 {0}
                    else {if case == 1 || (case > 8 && case <= 11) || case == 15 {0} else {1}};
                consumed = if Buggy == 3 && case == 1 {0}
                    else {if case == 1 || (case > 8 && case <= 12) || case == 15 {1} else {0}};
                returned = if Buggy == 4 && case == 1 {0}
                    else {if case == 1 || case == 9 || case == 11 || case == 15 {1} else {0}};
                marker = if Buggy == 5 && case == 2 {1}
                    else {if case == 1 || case == 9 || case == 11 || case == 15 {1} else {0}};
                owed = if Buggy == 6 && case == 11 {1}
                    else {if case == 1 || case == 9 || case == 15 {1} else {0}};
                observed = if Buggy == 7 && case == 1 {1} else {0};
            }
            invariant Admission: phase <= 1 || accepted ==
                if case == 1 || (case > 8 && case <= 11) || case == 15 {1} else {0};
            invariant RefusalIsIntact: phase <= 1 || accepted == 1 || intact == 1;
            invariant ReceiptConsumed: phase <= 1 || accepted == 0 || consumed == 1;
            invariant ReturnChoice: phase <= 1 || returned ==
                if case == 1 || case == 9 || case == 11 || case == 15 {1} else {0};
            invariant MarkerTogether: phase <= 1 || marker == returned;
            invariant OwedBelongsToRequest: phase <= 1 || owed ==
                if case == 1 || case == 9 || case == 15 {1} else {0};
            invariant ObservationPreserved: observed == 0;
        }
    }
}
