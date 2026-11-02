// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// In-place success lifts only older matching holds, without replacing credentials.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_signback_receipt_model() -> Model {
    aterm_spec::ty_model! {
        AccountSignBackReceipt {
            const Buggy = 0;
            var operation = 0; var case = 0; var phase = 0;
            var activity = 1; var ladder = 1; var foreign = 1; var fenced = 0;
            action Operation when (phase == 0 && 2 > operation) { operation = operation + 1; }
            action Case when (phase == 0 && 22 > case) { case = case + 1; }
            action Observe when (phase == 0) {
                phase = 1;
                activity = if Buggy == 1 {1}
                    else if (Buggy == 3 && case == 2) || (Buggy == 4 && case == 3)
                        || (Buggy == 5 && case == 5) || (Buggy == 6 && case == 6)
                        || (Buggy == 8 && case == 10) || (Buggy == 9 && case == 11)
                        || (Buggy == 10 && case == 12) || (Buggy == 11 && case == 15)
                        || (Buggy == 12 && case == 16) || (Buggy == 14 && case == 18)
                        || (Buggy == 17 && case == 19) || (Buggy == 18 && case == 20)
                        || (Buggy == 19 && case == 21) || (Buggy == 20 && case == 22) {0}
                    else if (Buggy == 7 && case == 7) || (Buggy == 13 && case == 14) {1}
                    else if case == 0 || case == 1 || case == 4 || case == 7 || case == 8 || case == 13 || case == 14 {0} else {1};
                ladder = if Buggy == 2 {1}
                    else if case == 0 || case == 1 || case == 4 || case == 7 || case == 8 || case == 13 || case == 14 {0} else {1};
                foreign = if Buggy == 15 {0} else {1};
                fenced = if Buggy == 16 {1} else {0};
            }
            invariant ActivityTracksReceipt: phase == 0 || activity ==
                if case == 0 || case == 1 || case == 4 || case == 7 || case == 8 || case == 13 || case == 14 {0} else {1};
            invariant LadderTracksReceipt: phase == 0 || ladder ==
                if case == 0 || case == 1 || case == 4 || case == 7 || case == 8 || case == 13 || case == 14 {0} else {1};
            invariant ForeignSurvives: foreign == 1;
            invariant NoAdditionalCredentialFence: fenced == 0;
        }
    }
}
