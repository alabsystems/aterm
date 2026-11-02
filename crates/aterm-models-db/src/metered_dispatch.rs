// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Composition of logical request binding, durable resource admission and a
/// fresh credential observation. Provider semantics and client authentication
/// remain trusted adapter/broker obligations outside this bounded model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn metered_dispatch_model() -> Model {
    aterm_spec::ty_model! {
        DbMeteredDispatch {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var entered = 0; var durable = 1; var exact = 1;
            action Fresh when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 12 {0} else {1}; entered = 1;
                durable = if Buggy == 10 {0} else {1}; exact = if Buggy == 11 {0} else {1};
            }
            action Retry when (scenario == 0) {
                scenario = 2; accepted = if Buggy == 13 {0} else {1}; entered = if Buggy == 2 {2} else {1};
            }
            action Window when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 1 {1} else {0}; entered = if Buggy == 1 {2} else {1};
            }
            action Account when (scenario == 0) {
                scenario = 4; accepted = if Buggy == 3 {1} else {0}; entered = if Buggy == 3 {2} else {1};
            }
            action Adapter when (scenario == 0) {
                scenario = 5; accepted = if Buggy == 4 {1} else {0}; entered = if Buggy == 4 {1} else {0};
            }
            action StoppedBefore when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 5 {1} else {0}; entered = if Buggy == 5 {1} else {0};
            }
            action CredentialAfter when (scenario == 0) {
                scenario = 7; accepted = if Buggy == 6 {1} else {0}; entered = if Buggy == 6 {1} else {0};
            }
            action StoppedAfter when (scenario == 0) {
                scenario = 8; accepted = if Buggy == 7 {1} else {0}; entered = if Buggy == 7 {1} else {0};
            }
            action Panic when (scenario == 0) {
                scenario = 9; accepted = 1; entered = if Buggy == 8 {2} else {1};
            }
            action Exhausted when (scenario == 0) {
                scenario = 10; accepted = if Buggy == 9 {1} else {0}; entered = if Buggy == 9 {1} else {0};
            }
            action Succession when (scenario == 0) {
                scenario = 11; accepted = if Buggy == 14 {1} else {0}; entered = if Buggy == 14 {2} else {1};
            }
            action LostRegistry when (scenario == 0) {
                scenario = 12; accepted = if Buggy == 15 {1} else {0}; entered = if Buggy == 15 {2} else {1};
            }
            action Expired when (scenario == 0) {
                scenario = 13; accepted = if Buggy == 16 {1} else {0}; entered = if Buggy == 16 {1} else {0};
            }
            action PendingEnrollment when (scenario == 0) {
                scenario = 14; accepted = if Buggy == 17 {0} else {1}; entered = if Buggy == 17 {0} else {1};
            }
            invariant FreshProgress: 1 > scenario || scenario > 1 || (accepted == 1 && entered == 1);
            invariant RetryObserved: 2 > scenario || scenario > 2 || accepted == 1;
            invariant NoReplay: entered <= 1;
            invariant CurrentBeforeEffect: 5 > scenario || scenario > 8 || entered == 0;
            invariant RefusalsHold: 3 > scenario || scenario == 9 || scenario > 13 || accepted == 0;
            invariant DurableBeforeEffect: entered == 0 || durable == 1;
            invariant ExactInputs: exact == 1;
            invariant DeadlineBeforeEffect: 13 > scenario || scenario > 13 || entered == 0;
            invariant PendingProgress: 14 > scenario || scenario > 14 || (accepted == 1 && entered == 1);
        }
    }
}
