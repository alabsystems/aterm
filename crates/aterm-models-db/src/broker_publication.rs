// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Reviewed refreshes keep grant identity and all other grants. Acceptance is
/// at a receiving store; this model promises no agreement across partitions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_publication_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerPublication {
            const Buggy = 0;
            var scenario = 0; var stored = 0; var identity = 1; var kept = 1; var used = 0;
            action Apply when (scenario == 0) {
                scenario = 1; stored = if Buggy == 1 {0} else {1}; used = if Buggy == 8 {1} else {0};
            }
            action Repeat when (scenario == 0) {
                scenario = 2; stored = if Buggy == 2 {2} else {1};
            }
            action Producer when (scenario == 0) {
                scenario = 3; stored = if Buggy == 3 {1} else {0}; identity = if Buggy == 3 {0} else {1};
            }
            action OtherGrant when (scenario == 0) {
                scenario = 4; stored = if Buggy == 4 {1} else {0}; kept = if Buggy == 4 {0} else {1};
            }
            action StaleBase when (scenario == 0) {
                scenario = 5; stored = if Buggy == 5 {1} else {0};
            }
            action MovedHead when (scenario == 0) {
                scenario = 6; stored = if Buggy == 6 {1} else {0};
            }
            action ForeignActor when (scenario == 0) {
                scenario = 7; stored = if Buggy == 7 {1} else {0};
            }
            invariant Progress: scenario == 0 || scenario > 2 || stored == 1;
            invariant ReviewedOnly: 3 > scenario || stored == 0;
            invariant IdentityStable: identity == 1;
            invariant OtherGrantsRetained: kept == 1;
            invariant Idempotent: stored <= 1;
            invariant NoSpend: used == 0;
        }
    }
}
