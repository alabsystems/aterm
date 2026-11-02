// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A live relay preserves historical facts across wall rollback while current
/// claims still require a fresh source sample and a bounded contact lease.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn relay_history_model() -> Model {
    aterm_spec::ty_model! {
        DbRelayHistory {
            const Buggy = 0;
            var case = 0; var phase = 0; var accepted = 0;
            var active = 0; var reachable = 0;
            var history = 0; var receipt = 0; var sample = 0;
            action Case when (phase == 0 && 20 > case) { case = case + 1; }
            action Read when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 && case == 1 {0}
                    else if (Buggy == 5 && case == 9) || (Buggy == 6 && case == 5)
                        || (Buggy == 7 && case == 13) {1}
                    else if case == 3 || case == 4 || case == 5 || case == 9 || case == 10
                        || (case > 12 && 18 > case) {0} else {1};
                active = if (Buggy == 8 && case == 7) || (Buggy == 9 && case == 18) {1}
                    else if case == 3 || case == 4 || case == 5 || case == 7
                        || case == 9 || case == 10 || (case > 12 && 20 > case)
                        || (Buggy == 1 && case == 1) {0} else {1};
                reachable = if Buggy == 10 && case == 8 {1}
                    else if case == 3 || case == 4 || case == 5 || case == 7 || case == 8
                        || case == 9 || case == 10 || case == 11 || (case > 12 && 20 > case)
                        || (Buggy == 1 && case == 1) {0} else {1};
                history = if Buggy == 2 && case == 1 {0}
                    else if case == 3 || case == 4 || case == 5 || case == 9 || case == 10
                        || (case > 12 && 18 > case) || (Buggy == 1 && case == 1) {0} else {1};
                receipt = if Buggy == 3 && case == 1 {0}
                    else if case == 3 || case == 4 || case == 5 || case == 9 || case == 10
                        || (case > 12 && 18 > case) || (Buggy == 1 && case == 1) {0} else {1};
                sample = if Buggy == 4 && case == 6 {0}
                    else if case == 3 || case == 4 || case == 5 || case == 9 || case == 10
                        || (case > 12 && 18 > case) || (Buggy == 1 && case == 1) {0} else {1};
            }
            invariant FreshHistoryDoesNotRefuseRead: phase == 0 || case == 3 || case == 4
                || case == 5 || case == 9 || case == 10 || (case > 12 && 18 > case) || accepted == 1;
            invariant OriginalHistoricalTimes: accepted == 0 || history == 1;
            invariant FreshHistoryKeepsInventoryReceipt: phase == 0 || case == 3 || case == 4
                || case == 5 || case == 9 || case == 10 || (case > 12 && 18 > case) || receipt == 1;
            invariant OriginalMonotonicSample: accepted == 0 || sample == 1;
            invariant FutureCurrentContactRefused: (9 > case || case > 10) || accepted == 0;
            invariant InvalidSourceSampleRefused: case <= 2 || (case > 5 && 15 > case)
                || case > 16 || accepted == 0;
            invariant IncompleteCurrentEvidenceRefused: case <= 12 || case == 15
                || case == 16 || case > 17 || accepted == 0;
            invariant SourceLeaseExpires: 7 > case || case > 7 || active == 0;
            invariant ReceiverClocksRemainRequired: (18 > case || case > 19) || active == 0;
            invariant ContactLeaseExpires: 8 > case || case > 8 || reachable == 0;
        }
    }
}
