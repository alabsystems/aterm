// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local token checks composed with the durable request namespace. Authenticated
/// policy storage and provider transport have their own, separate boundaries.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_capability_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerCapability {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var entered = 0;
            action Fresh when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 1 {0} else {1}; entered = 1;
            }
            action WrongToken when (scenario == 0) {
                scenario = 2; accepted = if Buggy == 2 {1} else {0}; entered = if Buggy == 2 {1} else {0};
            }
            action OtherBroker when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 3 {1} else {0}; entered = if Buggy == 3 {1} else {0};
            }
            action Revoked when (scenario == 0) {
                scenario = 4; accepted = if Buggy == 4 {1} else {0}; entered = if Buggy == 4 {1} else {0};
            }
            action Expired when (scenario == 0) {
                scenario = 5; accepted = if Buggy == 5 {1} else {0}; entered = if Buggy == 5 {1} else {0};
            }
            action PolicyChanged when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 6 {1} else {0}; entered = if Buggy == 6 {1} else {0};
            }
            action RevokedAfter when (scenario == 0) {
                scenario = 7; accepted = if Buggy == 7 {1} else {0}; entered = if Buggy == 7 {1} else {0};
            }
            action ExpiredAfter when (scenario == 0) {
                scenario = 8; accepted = if Buggy == 8 {1} else {0}; entered = if Buggy == 8 {1} else {0};
            }
            action Rotated when (scenario == 0) {
                scenario = 9; accepted = 1; entered = if Buggy == 9 {2} else {1};
            }
            action Restart when (scenario == 0) {
                scenario = 10; accepted = 1; entered = if Buggy == 10 {2} else {1};
            }
            action WrongBilling when (scenario == 0) {
                scenario = 11; accepted = if Buggy == 11 {1} else {0}; entered = if Buggy == 11 {1} else {0};
            }
            action PolicyAfter when (scenario == 0) {
                scenario = 12; accepted = if Buggy == 12 {1} else {0}; entered = if Buggy == 12 {1} else {0};
            }
            action IssueRollback when (scenario == 0) {
                scenario = 13; accepted = if Buggy == 13 {1} else {0}; entered = if Buggy == 13 {1} else {0};
            }
            action AbsentRollback when (scenario == 0) {
                scenario = 14; accepted = if Buggy == 14 {1} else {0}; entered = if Buggy == 14 {1} else {0};
            }
            action RevocationContended when (scenario == 0) {
                scenario = 15; accepted = if Buggy == 15 {1} else {0}; entered = if Buggy == 15 {1} else {0};
            }
            invariant Progress: (scenario > 1 && (9 > scenario || scenario > 10)) || scenario == 0 || (accepted == 1 && entered == 1);
            invariant Authorization: scenario == 0 || scenario == 1 || scenario == 9 || scenario == 10 || (accepted == 0 && entered == 0);
            invariant NoReplay: entered <= 1;
        }
    }
}
