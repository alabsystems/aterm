// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A selected write may follow a different entry's retirement cause only
/// after the entire authenticated cut. This is a bounded causal comparison,
/// not an account permission or a proof of the application's source policy.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_frontier_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryFrontier {
            const Buggy = 0;
            var scenario = 0; var done = 0; var proved = 0;
            action Case when (done == 0 && 24 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                proved = if scenario == 0 || scenario == 13 || scenario == 17 || scenario == 23 || scenario == 24 {
                    if Buggy == 6 {0} else {1}
                } else {
                    if (Buggy == 1 && scenario == 5)
                        || (Buggy == 2 && scenario == 3)
                        || (Buggy == 3 && scenario == 14)
                        || (Buggy == 4 && scenario == 11)
                        || (Buggy == 5 && scenario == 15) {1} else {0}
                };
            }
            invariant AuthenticatedCut: done == 0 || scenario == 0 || scenario == 13 || scenario == 17 || scenario == 23 || scenario == 24 || scenario == 3 || scenario == 11 || scenario == 14 || scenario == 15 || proved == 0;
            invariant SemanticWrite: done == 0 || (3 > scenario || scenario > 3) || proved == 0;
            invariant CompleteSelection: done == 0 || (11 > scenario || scenario > 11) || proved == 0;
            invariant WholeFrontier: done == 0 || (14 > scenario || scenario > 14) || proved == 0;
            invariant StrictlyAfterCut: done == 0 || (15 > scenario || scenario > 15) || proved == 0;
            invariant CausalProgressRecognized: done == 0 || ((scenario > 0) && (13 > scenario || scenario > 13) && (17 > scenario || scenario > 17) && (23 > scenario || scenario > 23) && (24 > scenario || scenario > 24)) || proved == 1;
        }
    }
}
