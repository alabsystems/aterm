// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Exact health-record ownership across two providers and three scopes, with three outcomes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scoped_health_model() -> Model {
    aterm_spec::ty_model! {
        ScopedHealth {
            const Buggy = 0;
            var observed = 0; var changed = 0; var outcome = 0; var phase = 0;
            var accepted = 0; var preserved = 0; var isolated = 0;
            action Observe when (phase == 0 && 5 > observed) { observed = observed + 1; }
            action Change when (phase == 0 && 5 > changed) { changed = changed + 1; }
            action Outcome when (phase == 0 && 2 > outcome) { outcome = outcome + 1; }
            action Commit when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 || observed > changed || changed > observed {1} else {0};
                preserved = if Buggy == 2 {0} else {1};
                isolated = if Buggy == 3 {0} else {1};
            }
            invariant ExactRecord: phase == 0 || accepted == (if observed > changed || changed > observed {1} else {0});
            invariant OtherRecords: phase == 0 || preserved == 1;
            invariant LegacyIsSeparate: phase == 0 || isolated == 1;
        }
    }
}
