// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Authentication precedes pure provider preparation, and a changed lease is
/// checked again before preparation can become a durable billable request.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_preparation_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerPreparation {
            const Buggy = 0;
            var scenario = 0; var parsed = 0; var entered = 0; var debit = 0;
            action Valid when (scenario == 0) {
                scenario = 1; parsed = 1; entered = if Buggy == 1 {0} else {1}; debit = 1;
            }
            action Invalid when (scenario == 0) {
                scenario = 2; parsed = 1; entered = if Buggy == 2 {1} else {0}; debit = entered;
            }
            action WrongToken when (scenario == 0) {
                scenario = 3; parsed = if Buggy == 3 {1} else {0};
            }
            action WrongAdapter when (scenario == 0) {
                scenario = 4; parsed = if Buggy == 4 {1} else {0};
            }
            action RevokedDuring when (scenario == 0) {
                scenario = 5; parsed = 1; debit = if Buggy == 5 {1} else {0};
            }
            action ExpiredDuring when (scenario == 0) {
                scenario = 6; parsed = 1; debit = if Buggy == 6 {1} else {0};
            }
            action Retry when (scenario == 0) {
                scenario = 7; parsed = 2; entered = if Buggy == 7 {2} else {1}; debit = 1;
            }
            action Panic when (scenario == 0) {
                scenario = 8; parsed = 1; debit = if Buggy == 8 {1} else {0};
            }
            invariant Progress: scenario > 1 || scenario == 0 || (parsed == 1 && entered == 1 && debit == 1);
            invariant AuthenticateBeforeParse: 3 > scenario || scenario > 4 || parsed == 0;
            invariant NoUnpreparedEffect: scenario == 0 || scenario == 1 || scenario == 7 || (entered == 0 && debit == 0);
            invariant NoReplay: entered <= 1 && debit <= 1;
        }
    }
}
