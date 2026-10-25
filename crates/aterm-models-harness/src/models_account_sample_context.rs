// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Only a fresh sample from the same managed login context may suppress a pool read.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sample_context_model() -> Model {
    aterm_spec::ty_model! {
        AccountSampleContext {
            const Buggy = 0;
            var phase = 0; var case = 1; var reads = 0; var preserved = 1;
            action AnotherCase when (phase == 0 && case <= 8) { case = case + 1; }
            action Choose when (phase == 0) { phase = 1; }
            action Poll when (phase == 1) {
                phase = 2;
                reads = if case == 1 { if Buggy == 1 {1} else {0} }
                    else if (case == 2 && Buggy == 2) || (case == 3 && Buggy == 3)
                        || (case == 4 && Buggy == 4) || (case == 5 && Buggy == 5)
                        || (case == 6 && Buggy == 6) || (case == 7 && Buggy == 7)
                        || (case == 8 && Buggy == 8) || (case == 9 && Buggy == 9) {0} else {1};
                preserved = if Buggy == 10 {0} else {1};
            }
            invariant CacheHit: phase <= 1 || case > 1 || reads == 0;
            invariant CacheMiss: phase <= 1 || case == 1 || reads == 1;
            invariant NoEvidenceNoErasure: preserved == 1;
        }
    }
}
