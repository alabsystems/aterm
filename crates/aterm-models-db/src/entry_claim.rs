// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A decoded claim requires the old selection to be re-established, and a
/// strict semantic successor. This proves the bounded local comparison only.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_claim_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryClaim {
            const Buggy = 0;
            var scenario = 0; var done = 0; var relation = 0;
            action Case when (done == 0 && 18 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                relation = if scenario == 0 || scenario == 16 {
                    if Buggy == 3 {0} else {2}
                } else {
                    if scenario == 1 || scenario == 2 || scenario == 11 || scenario == 15 {
                        if Buggy == 2 {2} else {1}
                    } else { if (Buggy == 1 && scenario == 3) || (Buggy == 4 && scenario == 17) {2} else {if Buggy == 5 && scenario == 18 {3} else {0}} }
                };
            }
            invariant OldSelectionRequired: done == 0 || scenario == 0 || scenario == 1 || scenario == 2 || scenario == 11 || scenario == 15 || scenario == 16 || relation == 0;
            invariant StableIsNotAdvance: done == 0 || ((1 > scenario || scenario > 1) && (2 > scenario || scenario > 2) && (11 > scenario || scenario > 11) && (15 > scenario || scenario > 15)) || relation == 1;
            invariant SemanticProgressRecognized: done == 0 || (scenario > 0 && (16 > scenario || scenario > 16)) || relation == 2;
        }
    }
}
