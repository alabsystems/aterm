// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A view needs authenticated custody even when every candidate is absent.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn entry_view_model() -> Model {
    aterm_spec::ty_model! {
        DbEntryView {
            const Buggy = 0;
            var scenario = 0; var done = 0; var outcome = 0; var entered = 0;
            var candidate = 0; var intent = 0; var held = 0; var after = 0;
            var free = 0; var kept = 1; var acquired = 0;
            action Case when (done == 0 && 12 > scenario) { scenario = scenario + 1; }
            action Read when (done == 0) {
                done = 1;
                outcome = if Buggy == 1 {1} else {
                    if (scenario > 2 && 11 > scenario) || scenario == 12 {3} else {1}
                };
                entered = if Buggy == 1 {1} else {
                    if (scenario > 2 && 11 > scenario) || scenario == 12 {0} else {1}
                };
                candidate = if scenario == 1 {1} else {
                    if scenario == 2 {if Buggy == 2 {2} else {3}} else {
                        if scenario == 0 || scenario == 11 {2} else {0}
                    }
                };
                intent = if scenario == 0 || scenario == 11 || (scenario == 2 && Buggy == 2) {1} else {0};
                held = if Buggy == 3 {0} else {if (scenario > 2 && 11 > scenario) || scenario == 12 {0} else {1}};
                after = if Buggy == 4 {0} else {if scenario > 2 && 6 > scenario {0} else {1}};
                free = if Buggy == 5 {0} else {1};
                kept = if Buggy == 6 {0} else {1};
                acquired = if Buggy == 7 {1} else {0};
            }
            invariant AuthenticatedOnly: done == 0 || outcome ==
                (if (scenario > 2 && 11 > scenario) || scenario == 12 {3} else {1});
            invariant CallbackOnlyAuthenticated: done == 0 || entered ==
                (if (scenario > 2 && 11 > scenario) || scenario == 12 {0} else {1});
            invariant CandidateOutcome: done == 0 || candidate ==
                (if scenario == 1 {1} else {if scenario == 2 {3} else {
                    if scenario == 0 || scenario == 11 {2} else {0}
                }});
            invariant NoFallbackFromUnavailable: scenario == 0 || scenario == 11 || intent == 0;
            invariant HeldThroughIntent: intent == 0 || held == 1;
            invariant OwnerRetains: done == 0 || after ==
                (if scenario > 2 && 6 > scenario {0} else {1});
            invariant OwnerReleases: done == 0 || free == 1;
            invariant ReadOnly: kept == 1;
            invariant NoReacquisition: acquired == 0;
        }
    }
}
