// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One attempted publication from an unbound Codex row, plus the projection of
/// historical proof after an identity/directory edit. This bounded case model
/// does not model a sequence of renewals or existing-binding preservation; the
/// latter is exercised separately by native tests. A named staging login needs
/// a matching report; its login address may be an explicit alias.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountIdentity {
            const Buggy = 0;
            var scenario = 0; var bound = 0; var proved = 0; var kept = 1;
            action Exact when (scenario == 0) {
                scenario = 1; bound = 1; proved = if Buggy == 1 {0} else {1};
            }
            action Alias when (scenario == 0) {
                scenario = 2; bound = if Buggy == 2 {0} else {1}; proved = 1;
            }
            action Wrong when (scenario == 0) {
                scenario = 3; bound = if Buggy == 3 {1} else {0}; kept = if Buggy == 6 {0} else {1};
            }
            action Missing when (scenario == 0) {
                scenario = 4; bound = if Buggy == 4 {1} else {0};
            }
            action Anonymous when (scenario == 0) {
                scenario = 5; bound = 1; proved = if Buggy == 5 {1} else {0};
            }
            action ChangedIdentity when (scenario == 0) {
                scenario = 6; bound = 1; proved = if Buggy == 7 {1} else {0};
            }
            action ChangedDirectory when (scenario == 0) {
                scenario = 7; bound = 1; proved = if Buggy == 8 {1} else {0};
            }
            action PolicyChanged when (scenario == 0) {
                scenario = 8; bound = if Buggy == 9 {1} else {0};
            }
            invariant NamedBindingIsProved: scenario > 4 || bound == 0 || proved == 1;
            invariant RefusalCannotBind: 3 > scenario || scenario > 4 || bound == 0;
            invariant ExplicitAliasWorks: scenario == 0 || scenario > 2 || bound == 1;
            invariant NoInventedOrStaleProof: 5 > scenario || proved == 0;
            invariant FailedCheckRetainsLogin: kept == 1;
            invariant PolicyChangeCannotBind: scenario <= 7 || scenario > 8 || bound == 0;
        }
    }
}
