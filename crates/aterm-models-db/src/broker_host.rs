// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded native lifecycle scenarios, including shutdown on either side of
/// the adapter entry gate. Arbitrary provider termination is not a model law.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn broker_host_model() -> Model {
    aterm_spec::ty_model! {
        DbBrokerHost {
            const Buggy = 0;
            var scenario = 0; var peak = 0; var workers = 0;
            var open = 1; var entered = 0; var debit = 0; var delivered = 0;
            action Saturated when (scenario == 0) {
                scenario = 1; peak = if Buggy == 1 {3} else {2}; workers = 2;
            }
            action StopWaiting when (scenario == 0) {
                scenario = 2; peak = 1; open = if Buggy == 7 {1} else {0}; workers = if Buggy == 2 {1} else {0};
                debit = if Buggy == 8 {1} else {0};
            }
            action StopAdmitted when (scenario == 0) {
                scenario = 3; peak = 1; open = 0; entered = 1;
                debit = if Buggy == 3 {0} else {1};
            }
            action StopAtGate when (scenario == 0) {
                scenario = 4; peak = 1; open = 0; debit = 1;
                entered = if Buggy == 4 {1} else {0};
            }
            action PanicReap when (scenario == 0) {
                scenario = 5; peak = 1; open = 0; entered = 2; debit = 2;
                workers = if Buggy == 5 {1} else {0};
            }
            action TokenChild when (scenario == 0) {
                scenario = 6; peak = 1; open = 0; entered = 1; debit = 1;
                delivered = if Buggy == 6 {0} else {1};
            }
            invariant Capacity: peak <= 2 && workers <= 2;
            invariant Drained: scenario == 0 || scenario == 1 || workers == 0;
            invariant StopBeforeEntry: 4 > scenario || scenario > 4 || entered == 0;
            invariant UnadmittedUncharged: scenario > 2 || debit == 0;
            invariant LiabilityRetained: 3 > scenario || debit == (if scenario == 5 {2} else {1});
            invariant PrivateDelivery: 6 > scenario || delivered == 1;
            invariant AdmissionClosed: 2 > scenario || open == 0;
        }
    }
}
