// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A recovery projection describes the candidates its captured advice actually ranked.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_progress_ranking_model() -> Model {
    aterm_spec::ty_model! {
        AccountProgressRanking {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var eligible = 2; var candidates = 2; var moved = 0;
            var low_rejected = 0; var candidate_low_rejected = 0; var unchanged = 1;
            action Case when (phase == 0 && 22 > case) { case = case + 1; }
            action Project when (phase == 0) {
                phase = 1;
                candidates = if case == 0 || case == 16 {2}
                    else if case == 4 || case == 7 || case == 11 || case == 12
                        || case == 13 || case == 17 || case > 19 {0} else {1};
                eligible = if (case == 0 || case == 16) {
                    if Buggy == 4 {0} else {2}
                } else if Buggy == 1 && case == 1 {0}
                    else if case == 4 || case == 7 || case == 11 || case == 12
                        || case == 13 || case == 17 || case > 19 {0} else {1};
                moved = if case == 1 || case == 2 || case == 3 || case == 5
                    || case == 6 || case == 8 || case == 9 || case == 10
                    || case == 14 || case == 15 || case == 18 {1} else {0};
                candidate_low_rejected = if case == 4 || case == 12 || case == 13 || case > 20 {1} else {0};
                low_rejected = if Buggy == 2 && case == 1 {1}
                    else if case == 4 || case == 12 || case == 13 || case > 20 {1} else {0};
                unchanged = if Buggy == 3 {0} else {1};
            }
            invariant ProjectionMatchesCandidates: phase == 0 || eligible == candidates;
            invariant SelectedDestinationCounted: moved == 0 || eligible == 1;
            invariant ExplanationsMatchCandidates: low_rejected == candidate_low_rejected;
            invariant CapturedImagePreserved: unchanged == 1;
            invariant UnjudgedEligibilityUnknown: phase == 0 || (case > 0 && 16 > case)
                || case > 16 || eligible == 2;
        }
    }
}
