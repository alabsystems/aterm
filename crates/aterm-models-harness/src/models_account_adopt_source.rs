// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Adoption retains metadata in the locally owned projection.
/// External authority and conflicting source metadata cannot be overridden locally,
/// even when an external file happens to match that projection's accounts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_adopt_source_model() -> Model {
    aterm_spec::ty_model! {
        AccountAdoptSource {
            const Buggy = 0;
            var case = 1; var phase = 0;
            var local = 0; var sources = 0; var granted = 0; var preserved = 1;
            action AnotherCase when (phase == 0 && case <= 12) { case = case + 1; }
            action Adopt when (phase == 0) {
                phase = 1;
                local = if case == 1 {if Buggy == 1 {0} else {1}}
                    else if case == 2 {if Buggy == 2 {0} else {1}}
                    else if case == 3 {if Buggy == 3 {1} else {0}}
                    else if case == 10 {if Buggy == 10 {0} else {1}}
                    else if case == 11 {if Buggy == 11 {0} else {1}}
                    else if case == 12 {if Buggy == 12 {0} else {1}}
                    else if (case == 4 && Buggy == 4) || (case == 5 && Buggy == 5)
                        || (case == 6 && Buggy == 6) || (case == 7 && Buggy == 7)
                        || (case == 8 && Buggy == 8) || (case == 9 && Buggy == 9)
                        || (case == 13 && Buggy == 13) {1} else {0};
                sources = if case == 3 {if Buggy == 15 {1} else {0}}
                    else if case == 2 {if Buggy == 14 {0} else {1}}
                    else if case == 10 || case == 11 || case == 12 {1}
                    else if case == 13 && Buggy == 16 {1} else {0};
                granted = if Buggy == 17 {1} else {0};
                preserved = if Buggy == 18 {0} else {1};
            }
            invariant AuthorityBoundary: local == 0 || case <= 2 || (case > 9 && 13 > case);
            invariant AdmissibleAdoption: phase == 0 || local == 1
                || (case > 2 && 10 > case) || case == 13;
            invariant OwnedSourcesRetainAdoption: local == 0 || case == 1
                || sources == 1;
            invariant ConflictingSourcesUnchanged: case <= 2 || case == 10
                || case == 11 || case == 12 || sources == 0;
            invariant NoCredentialAuthority: granted == 0;
            invariant ExternalInputPreserved: preserved == 1;
        }
    }
}
