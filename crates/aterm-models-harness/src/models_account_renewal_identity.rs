// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A captured successful status precedes this bounded commit observation.
/// Mode 0 is local, 1 has unrelated shared custody, and 2 uses a shared grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn renewal_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountRenewalIdentity {
            const Buggy = 0;
            var mode = 0; var credential = 0; var prior = 0; var phase = 0;
            var published = 0; var bound = 0; var unchanged = 1;
            var preserved = 1; var checkpoint = 0; var spent = 0;
            var unknown = 0; var calls = 0;
            action Mode when (phase == 0 && mode <= 1) { mode = mode + 1; }
            action Credential when (phase == 0 && credential <= 4) { credential = credential + 1; }
            action Prior when (phase == 0 && prior == 0) { prior = 1; }
            action Complete when (phase == 0) {
                phase = 1;
                published = if Buggy == 1 && credential == 2 {1}
                    else if Buggy == 2 && mode > 0 && credential == 5 {1}
                    else if Buggy == 3 && mode > 0 && credential == 4 {1}
                    else if Buggy == 4 && mode == 0 && credential == 0 {0}
                    else if Buggy == 5 && credential == 3 {0}
                    else if credential == 2 || (mode > 0 && (credential == 0 || credential == 4 || credential == 5)) {0} else {1};
                bound = if Buggy == 6 && prior == 1 {1}
                    else if credential == 2 || (mode > 0 && (credential == 0 || credential == 4 || credential == 5)) {0} else {1};
                unchanged = if Buggy == 7 {0}
                    else if credential == 2 || (mode > 0 && (credential == 0 || credential == 4 || credential == 5)) {1} else {0};
                preserved = if Buggy == 8 {0} else {1};
                checkpoint = if Buggy == 9 {1}
                    else if credential == 2 || (mode > 0 && (credential == 0 || credential == 4 || credential == 5)) {0} else {1};
                spent = if Buggy == 10 && credential == 2 {0} else if mode == 2 {1} else {0};
                unknown = if Buggy == 11 {0}
                    else if mode == 2 && (credential == 0 || credential == 2 || credential == 4 || credential == 5) {1} else {0};
                calls = 1;
            }
            action Replay when (phase == 1 && mode == 2) {
                phase = 2;
                calls = if Buggy == 12 {2} else {calls};
            }
            invariant Publication: phase == 0 || published == if credential == 2 || (mode > 0 && (credential == 0 || credential == 4 || credential == 5)) {0} else {1};
            invariant Binding: phase == 0 || bound == published;
            invariant Refusal: phase == 0 || unchanged == 1 - published;
            invariant Evidence: preserved == 1;
            invariant Checkpoint: checkpoint == published;
            invariant Consumption: spent == if phase > 0 && mode == 2 {1} else {0};
            invariant FailedOutcome: unknown == if phase > 0 && mode == 2 && published == 0 {1} else {0};
            invariant ProviderEntry: calls == if phase == 0 {0} else {1};
        }
    }
}
