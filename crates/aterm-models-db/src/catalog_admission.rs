// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Omitted catalog evidence cannot silently choose an older application table.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn catalog_admission_model() -> Model {
    aterm_spec::ty_model! {
        DbCatalogAdmission {
            const Buggy = 0;
            var scenario = 0; var done = 0; var accepted = 0; var present = 0;
            action Case when (done == 0 && 17 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                accepted = if Buggy == 2 || (Buggy == 4 && scenario > 14) {0} else {
                    if scenario <= 2 || scenario == 8 || scenario == 12 || scenario > 13 || (Buggy == 1 && (scenario <= 5 || scenario == 9 || scenario == 10 || scenario == 13)) || (Buggy == 3 && scenario == 11) {1} else {0}
                };
                present = if Buggy == 2 {0} else {if scenario == 1 || scenario == 12 {1} else {if scenario == 8 {if Buggy == 1 {0} else {1}} else {0}}};
            }
            invariant CompleteAvailable: done == 0 || (scenario > 2 && (8 > scenario || scenario > 8) && (12 > scenario || scenario > 12) && 14 > scenario) || accepted == 1;
            invariant PartialRefused: scenario <= 2 || scenario == 8 || scenario == 12 || scenario > 13 || accepted == 0;
            invariant DeclarationPreserved: done == 0 || ((1 > scenario || scenario > 1) && (8 > scenario || scenario > 8) && (12 > scenario || scenario > 12)) || present == 1;
            bound Cases: 0 <= scenario && scenario <= 17;
        }
    }
}
