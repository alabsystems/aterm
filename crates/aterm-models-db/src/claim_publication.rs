// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Whole-set publication by two local clients of one Commons writer. A stale second draft
/// cannot replace the first; a current lost reply is retryable, a superseded one is not.
/// This bounded model assumes the table writer serializes the admission check and append.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claim_publication_model() -> Model {
    aterm_spec::ty_model! {
        DbClaimPublication {
            const Buggy = 0;
            var phase = 0; var current = 0; var count = 0; var accepted = 0;
            action Initialize when (phase == 0) {
                phase = 1; current = 1; count = 1; accepted = if Buggy == 5 {0} else {1};
            }
            action Retry when (phase == 1) {
                phase = 2; accepted = 1; count = if Buggy == 1 {2} else {count};
            }
            action Stale when (phase == 1 || phase == 2) {
                phase = 3; accepted = if Buggy == 2 {1} else {0};
                current = if Buggy == 2 {2} else {current};
                count = if Buggy == 2 {count + 1} else {count};
            }
            action Advance when (phase == 3) {
                phase = 4; current = 2; count = 2; accepted = if Buggy == 6 {0} else {1};
            }
            action SupersededRetry when (phase == 4) {
                phase = 5; accepted = if Buggy == 3 {1} else {0};
            }
            action Skip when (phase == 1 || phase == 2) {
                phase = 6; accepted = if Buggy == 4 {1} else {0};
                current = if Buggy == 4 {2} else {current};
                count = if Buggy == 4 {count + 1} else {count};
            }
            invariant InitialProgress: 1 > phase || phase > 1 || (accepted == 1 && current == 1 && count == 1);
            invariant CurrentRetryIdempotent: 2 > phase || phase > 2 || (accepted == 1 && current == 1 && count == 1);
            invariant NoStaleReplacement: 3 > phase || phase > 3 || (accepted == 0 && current == 1 && count == 1);
            invariant FreshProgress: 4 > phase || phase > 4 || (accepted == 1 && current == 2 && count == 2);
            invariant NoSupersededSuccess: 5 > phase || phase > 5 || (accepted == 0 && current == 2 && count == 2);
            invariant NoSkippedRevision: 6 > phase || phase > 6 || (accepted == 0 && current == 1 && count == 1);
        }
    }
}
