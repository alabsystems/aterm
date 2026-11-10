// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A queued successor requires its exact current predecessor; an already
/// admitted successor instead recovers its original receipt without a write.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn publication_chain_model() -> Model {
    aterm_spec::ty_model! {
        DbPublicationChain {
            const Buggy = 0;
            var scenario = 0; var done = 0; var accepted = 0;
            var effects = 0; var same = 1;
            action Scenario when (done == 0 && 8 > scenario) { scenario = scenario + 1; }
            action Check when (done == 0) {
                done = 1;
                accepted = if scenario == 0 || scenario == 4 || scenario == 6 {
                    if Buggy == 3 && scenario == 4 {0} else {1}
                } else {
                    if (Buggy == 1 && scenario == 2) || (Buggy == 2 && scenario == 8) {1} else {0}
                };
                effects = if Buggy == 5 && scenario == 0 {0} else {if scenario == 0 || scenario == 6 ||
                    (Buggy == 1 && scenario == 2) || (Buggy == 2 && scenario == 8) {1} else {0}
                };
                same = if Buggy == 4 && scenario == 4 {0} else {1};
            }
            invariant NoUnadmittedWrite: scenario == 0 || scenario == 6 || effects == 0;
            invariant RefuseUnadmitted: done == 0 || scenario == 0 || scenario == 4 || scenario == 6 || accepted == 0;
            invariant AvailableProgress: done == 0 || (scenario > 0 && 4 > scenario) || scenario == 5 || scenario > 6 || accepted == 1;
            invariant OriginalReceipt: same == 1;
            invariant FreshPersistence: done == 0 || (scenario > 0 && 6 > scenario) || scenario > 6 || effects == 1;
            bound ScenarioDomain: 0 <= scenario && scenario <= 8;
        }
    }
}
