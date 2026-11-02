// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded native-backed scenarios for current-policy account selection. The
/// existing capability and billing models cover token use and durable debit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_selection_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerSelection {
            const Buggy = 0;
            var scenario = 0; var issued = 0; var entered = 0; var used = 0;
            action Unique when (scenario == 0) {
                scenario = 1; issued = if Buggy == 1 {0} else {1}; entered = 1; used = 1;
            }
            action Absent when (scenario == 0) {
                scenario = 2; issued = if Buggy == 2 {1} else {0};
            }
            action Ambiguous when (scenario == 0) {
                scenario = 3; issued = if Buggy == 3 {1} else {0};
            }
            action ForeignActor when (scenario == 0) {
                scenario = 4; issued = if Buggy == 4 {1} else {0};
            }
            action WrongAdapter when (scenario == 0) {
                scenario = 5; issued = if Buggy == 5 {1} else {0};
            }
            action Stale when (scenario == 0) {
                scenario = 6; issued = if Buggy == 6 {1} else {0};
            }
            action ChangedDuring when (scenario == 0) {
                scenario = 7; issued = if Buggy == 7 {1} else {0};
            }
            action Retry when (scenario == 0) {
                scenario = 8; issued = 1; entered = if Buggy == 8 {2} else {1}; used = 1;
            }
            invariant Progress: scenario == 0 || scenario > 1 || (issued == 1 && entered == 1 && used == 1);
            invariant ExactSelection: 2 > scenario || scenario == 8 || issued == 0;
            invariant NoReplay: entered <= 1 && used <= 1;
            invariant NoUnissuedEffect: issued == 1 || (entered == 0 && used == 0);
        }
    }
}
