// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A review may update only the account and credential generation it observed.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_review_observation_model() -> Model {
    aterm_spec::ty_model! {
        AccountReviewObservation {
            const Buggy = 0;
            var provider = 0; var location = 0; var outcome = 0; var case = 0; var phase = 0;
            var accepted = 0; var preserved = 1; var applied = 0;
            var foreign = 1; var fenced = 0;
            action Provider when (phase == 0 && provider == 0) { provider = 1; }
            action Location when (phase == 0 && location == 0) { location = 1; }
            action Outcome when (phase == 0 && 2 > outcome) { outcome = outcome + 1; }
            action Case when (phase == 0 && 16 > case) { case = case + 1; }
            action Record when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 {0}
                    else if Buggy == 2 || (Buggy == 7 && case > 13 && outcome == 2)
                        || (Buggy == 8 && case == 15 && outcome == 2)
                        || (Buggy == 9 && case == 16)
                        || case <= 1 || case == 10 || (case > 13 && 16 > case && 2 > outcome) {1} else {0};
                preserved = if Buggy == 3 || (Buggy == 7 && case > 13 && outcome == 2)
                    || (Buggy == 8 && case == 15 && outcome == 2)
                    || (Buggy == 9 && case == 16)
                    || case <= 1 || case == 10 || (case > 13 && 16 > case && 2 > outcome) {0} else {1};
                applied = if Buggy == 6 {0}
                    else if (Buggy == 7 && case > 13 && outcome == 2)
                        || (Buggy == 8 && case == 15 && outcome == 2)
                        || (Buggy == 9 && case == 16)
                        || case <= 1 || case == 10 || (case > 13 && 16 > case && 2 > outcome) {1} else {0};
                foreign = if Buggy == 4 {0} else {1};
                fenced = if Buggy == 5 {1} else {0};
            }
            invariant CurrentAccepted: phase == 0 || (case > 1 && 10 > case)
                || (case > 10 && 14 > case) || (case > 13 && outcome == 2) || case == 16 || accepted == 1;
            invariant CurrentApplied: phase == 0 || (case > 1 && 10 > case)
                || (case > 10 && 14 > case) || (case > 13 && outcome == 2) || case == 16 || applied == 1;
            invariant StaleRefused: phase == 0 || case <= 1 || case == 10
                || (case > 13 && 16 > case && 2 > outcome) || accepted == 0;
            invariant RefusalPreserves: phase == 0 || case <= 1 || case == 10
                || (case > 13 && 16 > case && 2 > outcome) || preserved == 1;
            invariant ForeignPreserved: foreign == 1;
            invariant NoCredentialFence: fenced == 0;
        }
    }
}
