// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Current signed-base refresh is inert, retains identity and cannot mint a new
/// retry namespace. Native conformance includes an unsafe manual reconstruction.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_refresh_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerRefresh {
            const Buggy = 0;
            var scenario = 0; var drafted = 0; var identity = 1; var retained = 1;
            var used = 0; var entered = 0;
            action Changed when (scenario == 0) {
                scenario = 1; drafted = if Buggy == 8 {0} else {1};
                identity = if Buggy == 1 {0} else {1}; retained = if Buggy == 2 {0} else {1};
            }
            action Unchanged when (scenario == 0) {
                scenario = 2; drafted = 1; used = if Buggy == 9 {1} else {0};
            }
            action Absent when (scenario == 0) {
                scenario = 3; drafted = if Buggy == 3 {1} else {0};
            }
            action ForeignActor when (scenario == 0) {
                scenario = 4; drafted = if Buggy == 4 {1} else {0};
            }
            action AdapterChanged when (scenario == 0) {
                scenario = 5; drafted = if Buggy == 5 {1} else {0};
            }
            action PolicyChanged when (scenario == 0) {
                scenario = 6; drafted = if Buggy == 6 {1} else {0};
            }
            action Retry when (scenario == 0) {
                scenario = 7; drafted = 1; used = if Buggy == 7 {2} else {1}; entered = if Buggy == 7 {2} else {1};
            }
            invariant Progress: scenario == 0 || (scenario > 2 && 7 > scenario) || drafted == 1;
            invariant AuthorizedBase: 3 > scenario || scenario > 6 || drafted == 0;
            invariant IdentityStable: identity == 1;
            invariant OtherGrantsRetained: retained == 1;
            invariant DraftDoesNotSpend: scenario == 7 || (used == 0 && entered == 0);
            invariant NoReplay: used <= 1 && entered <= 1;
        }
    }
}
