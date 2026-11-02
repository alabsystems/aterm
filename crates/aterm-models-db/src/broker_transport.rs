// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Socket framing composed with actual broker admission. The model bounds one
/// logical request and its retry; it does not prove OS isolation or provider I/O.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_transport_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerTransport {
            const Buggy = 0;
            var scenario = 0; var entered = 0; var debit = 0; var returned = 0;
            action Fresh when (scenario == 0) {
                scenario = 1; entered = 1; debit = 1; returned = if Buggy == 1 {0} else {1};
            }
            action Trailing when (scenario == 0) {
                scenario = 2; entered = if Buggy == 2 {1} else {0}; debit = 0;
            }
            action ShortHeader when (scenario == 0) {
                scenario = 3; entered = if Buggy == 3 {1} else {0}; debit = 0;
            }
            action ShortBody when (scenario == 0) {
                scenario = 4; entered = if Buggy == 4 {1} else {0}; debit = 0;
            }
            action Oversize when (scenario == 0) {
                scenario = 5; entered = if Buggy == 5 {1} else {0}; debit = 0;
            }
            action Version when (scenario == 0) {
                scenario = 6; entered = if Buggy == 6 {1} else {0}; debit = 0;
            }
            action Expired when (scenario == 0) {
                scenario = 7; entered = if Buggy == 7 {1} else {0}; debit = 0;
            }
            action WrongToken when (scenario == 0) {
                scenario = 8; entered = if Buggy == 8 {1} else {0}; debit = 0;
            }
            action WrongAdapter when (scenario == 0) {
                scenario = 9; entered = if Buggy == 9 {1} else {0}; debit = 0;
            }
            action LostReply when (scenario == 0) {
                scenario = 10; entered = 1; debit = if Buggy == 10 {0} else {1}; returned = 0;
            }
            action Retry when (scenario == 0) {
                scenario = 11; entered = if Buggy == 11 {2} else {1}; debit = 1; returned = 1;
            }
            action NoEof when (scenario == 0) {
                scenario = 12; entered = if Buggy == 12 {1} else {0}; debit = 0;
            }
            invariant FrameBeforeEffect: scenario == 0 || scenario == 1 || scenario == 10 || scenario == 11 || (entered == 0 && debit == 0 && returned == 0);
            invariant RetainedCharge: scenario > 10 || 10 > scenario || debit == 1;
            invariant Progress: ((scenario > 1 || 1 > scenario) && (scenario > 11 || 11 > scenario)) || (entered == 1 && debit == 1 && returned == 1);
            invariant NoReplay: entered <= 1;
        }
    }
}
