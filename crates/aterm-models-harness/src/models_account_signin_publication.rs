// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// File-backed sign-in publication after a provider check, with first and replacement bindings.
/// Codex observes known email/user contradictions as well as presence; opaque
/// claims remain compatible. This neither authenticates tokens nor leases a file.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn signin_publication_model() -> Model {
    aterm_spec::ty_model! {
        AccountSignInPublication {
            const Buggy = 0;
            var provider = 0; var prior = 0; var fault = 0; var phase = 0;
            var published = 0; var changed = 0; var checkpoint = 0; var retained = 1;
            action AnotherProvider when (phase == 0 && provider <= 1) { provider = provider + 1; }
            action PriorBinding when (phase == 0 && prior == 0) { prior = 1; }
            action AnotherFault when (phase == 0 && fault <= 6) { fault = fault + 1; }
            action Publish when (phase == 0) {
                phase = 1;
                published = if fault == 0 || fault > 5 || (provider > 0 && fault > 2) {if Buggy == 3 {0} else {1}}
                    else {if (fault == 1 && Buggy == 1) || (fault == 2 && Buggy == 2)
                        || (fault == 3 && Buggy == 9) || (fault == 4 && Buggy == 10)
                        || (fault == 5 && Buggy == 11) {1} else {0}};
                changed = if fault == 0 || fault > 5 || (provider > 0 && fault > 2) {if Buggy == 7 {0} else {1}}
                    else {if Buggy == 4 {1} else {0}};
                checkpoint = if fault == 0 || fault > 5 || (provider > 0 && fault > 2) {if Buggy == 8 {0} else {1}}
                    else {if Buggy == 5 {1} else {0}};
                retained = if Buggy == 6 {0} else {1};
            }
            invariant ResultPolicy: phase == 0 || published == if fault == 0 || fault > 5 || (provider > 0 && fault > 2) {1} else {0};
            invariant CompleteRecord: phase == 0 || published == 0 || (changed == 1 && checkpoint == 1);
            invariant RefusalPreserves: phase == 0 || published == 1 || (changed == 0 && checkpoint == 0);
            invariant EvidenceRetained: retained == 1;
        }
    }
}
