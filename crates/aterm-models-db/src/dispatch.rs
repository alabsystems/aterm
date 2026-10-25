// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One invocation lifecycle, including persistent crash states and subsequent
/// retries. Invocation means adapter entry, not a proven remote side effect.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resource_dispatch_model() -> Model {
    aterm_spec::ty_model! {
        DbResourceDispatch {
            const Buggy = 0;
            var phase = 0; var charged = 0; var entered = 0; var observed = 0;
            var answer = 0; var count = 0; var log = 0; var witness = 0; var damaged = 0; var legacy = 0;
            var retried = 0; var reentered = 0; var same = 1; var wrong = 0; var refused = 0;
            action Admit when (phase == 0) {
                phase = 1; charged = 1; log = 1; witness = if Buggy == 1 {0} else {1};
            }
            action Complete when (phase == 1) {
                phase = 2; entered = if Buggy == 7 {0} else {1}; answer = 1;
            }
            action Reject when (phase == 1) {
                phase = 2; entered = 1; answer = 2;
            }
            action Unknown when (phase == 1) {
                phase = 2; entered = 1;
            }
            action Record when (phase == 2) {
                phase = if answer == 0 {4} else {3}; observed = answer;
                count = if answer == 0 {0} else {if Buggy == 9 {2} else {1}};
                log = if answer == 0 {1} else {2}; witness = if answer == 0 {1} else {2};
                charged = if Buggy == 5 && answer == 2 {0} else {charged};
            }
            action CrashBefore when (phase == 1) {
                phase = 4; answer = 0; charged = if Buggy == 11 {0} else {charged};
            }
            action CrashAfter when (phase == 2) {
                phase = 4; answer = 0; count = if Buggy == 4 {1} else {0};
            }
            action Legacy when (phase == 0) {
                phase = 5; charged = if Buggy == 10 {2} else {1}; log = 1; witness = 1;
                entered = if Buggy == 6 {1} else {0}; legacy = 1;
            }
            action NotAdmitted when (phase == 0) {
                phase = 6;
            }
            action LoseHistory when (phase > 0 && phase <= 5) {
                phase = 6; damaged = 1; log = 0; answer = 0; observed = 0; count = 0;
            }
            action Retry when (phase > 0 && retried == 0) {
                retried = 1; reentered = if Buggy == 2 {1} else {0};
                entered = if Buggy == 2 {entered + 1} else {entered};
                same = if Buggy == 3 {0} else {1};
            }
            action WrongOperation when (phase > 0 && wrong == 0) {
                wrong = 1; refused = if Buggy == 8 {0} else {1};
            }
            invariant LiabilityBeforeEntry: entered == 0 || (charged == 1 && witness > 0);
            invariant WitnessKeepsCharge: witness == 0 || charged == 1;
            invariant AtMostOneEntry: entered <= 1;
            invariant NoEntryOnRetry: reentered == 0;
            invariant SameReceipt: retried == 0 || same == 1;
            invariant NoInventedObservation: phase <= 3 || count == 0;
            invariant BoundedLiability: charged <= 1;
            invariant NoLegacyEntry: legacy == 0 || entered == 0;
            invariant AdapterWasEntered: phase <= 1 || phase > 2 || entered == 1;
            invariant OneObservation: count <= 1;
            invariant ObservationAfterIntent: count == 0 || (entered == 1 && witness > 1 && log > 1);
            invariant ChangedOperationRefused: wrong == 0 || refused == 1;
        }
    }
}
