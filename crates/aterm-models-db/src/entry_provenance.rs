// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Real local entry reads over envelope, log, resealed and concurrent histories.
/// This proves the bounded provenance decisions, not automatic reactivation or
/// the absence of an undelivered remote write.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_provenance_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryProvenance {
            const Buggy = 0;
            var scenario = 0; var done = 0; var log = 0; var supersedes = 0;
            action Case when (done == 0 && 16 > scenario) { scenario = scenario + 1; }
            action Observe when (done == 0) {
                done = 1;
                log = if Buggy == 1 && scenario == 2 {1} else {
                    if scenario == 0 || scenario == 2 {0} else {1}
                };
                supersedes = if scenario == 4 || scenario == 5 || scenario == 15 || scenario == 16 {
                    if Buggy == 4 {0} else {1}
                } else {
                    if (Buggy == 2 && scenario == 6) || (Buggy == 3 && scenario == 8) || (Buggy == 5 && scenario == 14) {1} else {0}
                };
            }
            invariant ActualSource: done == 0 || log == (if scenario == 0 || scenario == 2 {0} else {1});
            invariant OnlyCausalSemanticChange: scenario == 4 || scenario == 5 || scenario == 15 || scenario == 16 || supersedes == 0;
            invariant SemanticChangeRecognized: done == 0 || ((4 > scenario || scenario > 4) && (5 > scenario || scenario > 5) && (15 > scenario || scenario > 15) && (16 > scenario || scenario > 16)) || supersedes == 1;
        }
    }
}
