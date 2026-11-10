// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A borrowed vault look keeps custody with its owner across entry admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_read_context_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryReadContext {
            const Buggy = 0;
            var scenario = 0; var done = 0; var outcome = 0; var entered = 0;
            var candidate = 0; var intent = 0; var held = 0; var after = 0;
            var free = 0; var kept = 1;
            action Case when (done == 0 && 8 > scenario) { scenario = scenario + 1; }
            action Admit when (done == 0) {
                done = 1;
                outcome = if Buggy == 1 {1} else {
                    if scenario == 2 {4} else {
                        if scenario > 2 && 6 > scenario {3} else {1}
                    }
                };
                entered = if Buggy == 7 {1} else {
                    if scenario == 2 || (scenario > 2 && 6 > scenario) {0} else {1}
                };
                candidate = if scenario == 6 {if Buggy == 3 {2} else {3}} else {
                    if scenario == 7 {if Buggy == 6 {1} else {2}} else {
                        if scenario == 8 {4} else {if 2 > scenario {1} else {0}}
                    }
                };
                intent = if 2 > scenario || scenario == 7 || (Buggy == 3 && scenario == 6) {1} else {0};
                held = if Buggy == 2 {0} else {
                    if 2 > scenario || scenario > 5 {1} else {0}
                };
                after = if Buggy == 4 || (scenario > 2 && 6 > scenario) {0} else {1};
                free = if Buggy == 8 {0} else {1};
                kept = if Buggy == 5 {0} else {1};
            }
            invariant AdmissionOutcome: done == 0 || outcome ==
                (if scenario == 2 {4} else {if scenario > 2 && 6 > scenario {3} else {1}});
            invariant CallbackOnlyCurrent: done == 0 || entered ==
                (if scenario == 2 || (scenario > 2 && 6 > scenario) {0} else {1});
            invariant CandidateOutcome: done == 0 || candidate ==
                (if scenario == 6 {3} else {if scenario == 7 {2} else {
                    if scenario == 8 {4} else {if 2 > scenario {1} else {0}}
                }});
            invariant HeldThroughIntent: intent == 0 || held == 1;
            invariant OwnerKeepsGuard: done == 0 || after ==
                (if scenario > 2 && 6 > scenario {0} else {1});
            invariant OwnerCanRelease: done == 0 || free == 1;
            invariant VaultReadOnly: kept == 1;
        }
    }
}
