// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local prepared-intent ownership and recovery. Source authentication is a
/// separate protocol; this model grants no vault or provider authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_publication_phases_model() -> Model {
    aterm_spec::ty_model! {
        AccountPublicationPhases {
            const Buggy = 0;
            var scenario = 0; var done = 0;
            var writer = 1; var unchanged = 1; var fenced = 1;
            var fresh = 1; var consumed = 1; var outcome = 1;
            action Scenario when (done == 0 && 9 > scenario) { scenario = scenario + 1; }
            action Check when (done == 0) {
                done = 1;
                writer = if Buggy == 2 && scenario == 6 {0} else {1};
                unchanged = if Buggy == 1 && scenario == 0 {0} else {1};
                fenced = if Buggy == 1 && scenario == 0 {0} else {1};
                fresh = if (Buggy == 3 && scenario == 3) || (Buggy == 5 && scenario == 2) {0} else {1};
                consumed = if Buggy == 4 && scenario == 4 {0} else {1};
                outcome = if (Buggy == 6 && scenario == 0) || (Buggy == 7 && scenario == 9) {0} else {1};
            }
            invariant MatchingWriter: writer == 1;
            invariant NoImagesBeforeReplay: unchanged == 1;
            invariant PendingFencesReaders: fenced == 1;
            invariant FreshInputs: fresh == 1;
            invariant ConsumedIntentIsFinal: consumed == 1;
            invariant ExpectedOutcome: outcome == 1;
            bound ScenarioDomain: 0 <= scenario && scenario <= 9;
        }
    }
}
