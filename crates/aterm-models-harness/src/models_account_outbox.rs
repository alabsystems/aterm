// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local receipt recovery, bounded queue admission and stale completion
/// isolation. Application delivery and remote convergence are separate laws.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_publication_outbox_model() -> Model {
    aterm_spec::ty_model! {
        AccountPublicationOutbox {
            const Buggy = 0;
            var scenario = 0; var done = 0;
            var retained = 1; var capacity = 1; var isolated = 1;
            action Scenario when (done == 0 && 2 > scenario) { scenario = scenario + 1; }
            action Check when (done == 0) {
                done = 1;
                retained = if Buggy == 1 && scenario == 0 {0} else {1};
                capacity = if Buggy == 2 && scenario == 1 {0} else {1};
                isolated = if Buggy == 3 && scenario == 2 {0} else {1};
            }
            invariant ReceiptRetainsPublication: retained == 1;
            invariant CapacityPreservesPending: capacity == 1;
            invariant StaleCompletionIsIsolated: isolated == 1;
            bound ScenarioDomain: 0 <= scenario && scenario <= 2;
        }
    }
}
