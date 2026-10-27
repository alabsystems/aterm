// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local batch admission before/after session open in both repair directions.
/// Configuration writers are independent; this is not a continuous policy lease.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn repair_policy_model() -> Model {
    aterm_spec::ty_model! {
        DbRepairPolicy {
            const Buggy = 0;
            // Policy: unchanged, equivalent, unrelated exclusion, disabled,
            // table excluded, catalog excluded, malformed, unknown setting.
            var policy = 0; var receiving = 0; var point = 0; var phase = 0;
            var accepted = 0; var receipts = 0; var retained = 1;
            action NextPolicy when (phase == 0 && 7 > policy) { policy = policy + 1; }
            action Receiving when (phase == 0 && receiving == 0) { receiving = 1; }
            action AfterOpen when (phase == 0 && point == 0) { point = 1; }
            action Attempt when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 {1} else if Buggy == 2 && policy == 1 {0}
                    else if policy <= 1 {1} else {0};
                receipts = if Buggy == 5 {1} else if policy <= 1 {1} else {0};
                retained = if Buggy == 4 {0} else if Buggy == 9 && receiving == 0 {2}
                    else if receiving == 1 && policy <= 1 {
                        if Buggy == 8 {1} else {2}
                    } else {1};
            }
            action Retry when (phase == 1) {
                phase = 2;
                accepted = if Buggy == 3 && policy == 2 {0} else if Buggy == 7 {1}
                    else if policy <= 2 {1} else {0};
                receipts = if policy <= 2 {1} else {0};
                retained = if receiving == 1 && policy <= 2 {2} else {retained};
            }
            action Restore when (phase == 2) {
                phase = 3; accepted = if Buggy == 6 {0} else {1}; receipts = 1;
                retained = if receiving == 1 {2} else {retained};
            }
            invariant LateChangeRefuses: 1 > phase || phase > 1 || policy <= 1 || accepted == 0;
            invariant EquivalentProgress: 1 > phase || phase > 1 || policy > 1 || accepted == 1;
            invariant FreshRoundPolicy: 2 > phase || phase > 2 || accepted == if policy <= 2 {1} else {0};
            invariant RestoreProgress: 3 > phase || accepted == 1;
            invariant HistoryPreserved: retained > 0;
            invariant NoFalseReceipt: receipts == 0 || accepted == 1;
            invariant ReceiptHasStorage: receipts == 0 || receiving == 0 || retained == 2;
            invariant OutgoingReadOnly: receiving == 1 || retained == 1;
        }
    }
}
