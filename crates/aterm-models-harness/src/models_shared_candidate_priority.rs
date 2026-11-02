// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded history and authority partitions for shared candidate ordering.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_candidate_priority_model() -> Model {
    aterm_spec::ty_model! {
        SharedCandidatePriority {
            const Buggy = 0;
            var phase = 0; var first = 0; var count = 0; var kept = 1;
            action Tie when (phase == 0) {
                phase = 1; first = if Buggy == 1 {2} else {1}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            action Unseen when (phase == 0) {
                phase = 2; first = if Buggy == 2 {1} else {2}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            action Oldest when (phase == 0) {
                phase = 3; first = if Buggy == 3 {2} else {1}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            action Reissue when (phase == 0) {
                phase = 4; first = if Buggy == 4 {1} else {2}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            action Unattributed when (phase == 0) {
                phase = 5; first = if Buggy == 5 {2} else {1}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            action Refused when (phase == 0) {
                phase = 6; first = if Buggy == 6 {1} else {2};
                count = if Buggy == 6 {2} else {1};
                kept = if Buggy == 8 {0} else {1};
            }
            action Context when (phase == 0) {
                phase = 7; first = if Buggy == 7 {1} else {2}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            action ContextHistory when (phase == 0) {
                phase = 8; first = if Buggy == 9 {1} else {2}; count = 2;
                kept = if Buggy == 8 {0} else {1};
            }
            invariant StableTie: 1 > phase || phase > 1 || first == 1;
            invariant UnseenFirst: 2 > phase || phase > 2 || first == 2;
            invariant OldestFirst: 3 > phase || phase > 3 || first == 1;
            invariant ReissuedIsUnseen: 4 > phase || phase > 4 || first == 2;
            invariant HistoryNeedsAttribution: 5 > phase || phase > 5 || first == 1;
            invariant RefusedIsAbsent: 6 > phase || phase > 6 || (first == 2 && count == 1);
            invariant FullSourceTie: 7 > phase || phase > 7 || first == 2;
            invariant FullSourceHistory: 8 > phase || phase > 8 || first == 2;
            invariant RankingIsReadOnly: kept == 1;
        }
    }
}

/// Repeated admissions for two stable, eligible candidates with retained history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_candidate_progress_model() -> Model {
    aterm_spec::ty_model! {
        SharedCandidateProgress {
            const Buggy = 0;
            var a = 0; var b = 0; var newest = 0; var last = 0; var repeated = 0;
            action A when (Buggy == 1 || a == 0 || (b == 1 && newest == 2)) {
                a = if Buggy == 2 {0} else {1};
                newest = if Buggy == 3 {2} else {1};
                repeated = if last == 1 {1} else {0}; last = 1;
            }
            action B when (a == 1 && (b == 0 || newest == 1)) {
                b = 1; newest = if Buggy == 4 {1} else {2};
                repeated = if last == 2 {1} else {0}; last = 2;
            }
            invariant BothReceiveTurns: repeated == 0;
        }
    }
}
