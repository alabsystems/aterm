// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A snapshot publication attempt, optional snapshot loss, then a read and transaction.
/// The witness and legacy ledger survive the modeled loss. This model does not claim
/// filesystem flush correctness, cross-file atomicity, or protection after all evidence
/// is deleted. InterruptedSave ends before a snapshot can be published.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_state_retention_model() -> Model {
    aterm_spec::ty_model! {
        AccountStateRetention {
            const Buggy = 0;
            var phase = 0; var kind = 0; var attempted = 0;
            var witness = 0; var ledger = 0; var present = 0;
            var accepted = 0; var edited = 0;
            action Save when (phase == 0) {
                phase = 1; kind = 1; attempted = 1; present = 1;
                witness = if Buggy == 1 {0} else {1};
            }
            action InterruptedSave when (phase == 0) {
                phase = 1; kind = 1; attempted = 1;
                witness = if Buggy == 1 {0} else {1};
            }
            action Legacy when (phase == 0) {
                phase = 1; kind = 2; ledger = 1; present = 1;
            }
            action StatsOnly when (phase == 0) {
                phase = 1; kind = 3;
            }
            action EventOnly when (phase == 0) {
                phase = 1; kind = 4;
                attempted = if Buggy == 6 {0} else {1};
                witness = if Buggy == 1 || Buggy == 6 {0} else {1};
                present = if Buggy == 6 {0} else {1}; ledger = 1;
            }
            action LoseSnapshot when (phase == 1 && present == 1) {
                phase = 2; present = 0;
            }
            action Read when (phase <= 2) {
                phase = 3;
                accepted = if present == 1 {1}
                    else if (witness == 0 || Buggy == 2) && (ledger == 0 || Buggy == 3) {
                        if Buggy == 5 {0} else {1}
                    } else {0};
            }
            action Edit when (phase == 3) {
                phase = 4;
                edited = if accepted == 1 || Buggy == 4 {1} else {0};
                attempted = if accepted == 1 {1} else {attempted};
                witness = if accepted == 1 {1} else {witness};
                present = if accepted == 1 {1} else {present};
            }
            invariant AttemptRetained: attempted == 0 || witness == 1;
            invariant WitnessLossRefused: phase <= 2 || present == 1 || witness == 0 || accepted == 0;
            invariant LegacyLossRefused: phase <= 2 || present == 1 || ledger == 0 || accepted == 0;
            invariant RefusalNotEdited: edited == 0 || accepted == 1;
            invariant FreshAllowed: phase <= 2 || present == 1 || witness == 1 || ledger == 1 || accepted == 1;
            invariant FirstEventPublished: kind <= 3 || phase == 0 || phase > 1 || present == 1;
        }
    }
}
