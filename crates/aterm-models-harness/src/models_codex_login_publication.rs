// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// New-login publication admits coherent token pairs, retains refusals, and
/// records independent login ownership and only supported OAuth provenance.
/// The fixture covers validation before publication, not filesystem failures.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_login_publication_model() -> Model {
    aterm_spec::ty_model! {
        CodexLoginPublication {
            const Buggy = 0;
            var case = 0; var phase = 0; var result = 0; var posted = 0;
            var kept = 1; var fresh = 1; var bound = 1;
            action AnotherCase when (phase == 0 && case <= 22) {case = case + 1;}
            action Exchange when (phase == 0) {
                phase = 1;
                result = if Buggy == 1 {1} else {if Buggy == 2 {0} else {
                    if case == 1 || case == 2 || case == 6 || case == 7 || case == 9
                        || case == 14 || case == 15 || case > 17 {0} else {1}
                }};
                posted = if case <= 17 || case > 21 || Buggy == 6 {1} else {0};
                kept = if Buggy == 3 {0} else {1};
                fresh = if Buggy == 4 {0} else {1};
                bound = if Buggy == 5 {0} else {1};
            }
            invariant Admission: phase == 0 || result ==
                if case == 1 || case == 2 || case == 6 || case == 7 || case == 9
                    || case == 14 || case == 15 || case > 17 {0} else {1};
            invariant RequestBoundary: phase == 0 || posted == if case <= 17 || case > 21 {1} else {0};
            invariant PreserveRejected: phase == 0 || result == 1 || kept == 1;
            invariant FreshOwner: phase == 0 || result == 0 || fresh == 1;
            invariant Provenance: phase == 0 || result == 0 || bound == 1;
        }
    }
}
