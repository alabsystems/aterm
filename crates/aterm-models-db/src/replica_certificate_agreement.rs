// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Replica composition cannot erase different certificates under one actor fence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn replica_certificate_agreement_model() -> Model {
    aterm_spec::ty_model! {
        DbReplicaCertificateAgreement {
            const Buggy = 0;
            var scenario = 0; var done = 0; var accepted = 0; var proofs = 0;
            action Case when (done == 0 && 5 > scenario) { scenario = scenario + 1; }
            action Combine when (done == 0) {
                done = 1;
                accepted = if Buggy == 2 {0} else {if scenario <= 1 || scenario == 5 || Buggy == 1 {1} else {0}};
                proofs = if Buggy == 2 {0} else {if scenario == 5 {if Buggy == 3 {1} else {2}} else {if scenario == 1 || (Buggy == 1 && scenario > 1) {1} else {0}}};
            }
            invariant Agreement: scenario <= 1 || scenario == 5 || accepted == 0;
            invariant Available: done == 0 || (scenario > 1 && 5 > scenario) || accepted == 1;
            invariant Retained: accepted == 0 || proofs == (if scenario == 0 {0} else {if scenario == 5 {2} else {1}});
            bound Cases: 0 <= scenario && scenario <= 5;
        }
    }
}
