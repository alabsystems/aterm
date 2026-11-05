// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_live_admission_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeLiveAdmission {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var executed = 0; var waited = 0; var binding = 1; var captured = 1;
            action NextCase when (phase == 0 && 9 > case) { case = case + 1; }
            action Attempt when (phase == 0) {
                phase = 1;
                executed = if case == 0 || case == 4 {if Buggy == 2 {0} else {1}} else {if Buggy == 1 {1} else {0}};
                waited = if case == 0 || case == 4 {0} else {if Buggy == 5 {0} else {1}};
                binding = if Buggy == 3 {0} else {1};
                captured = if Buggy == 4 {0} else {1};
            }
            invariant Admission: phase == 0 || case == 0 || case == 4 || executed == 0;
            invariant Progress: phase == 0 || (case > 0 && (4 > case || case > 4)) || executed == 1;
            invariant RetainedWait: phase == 0 || case == 0 || case == 4 || waited == 1;
            invariant CurrentBinding: binding == 1;
            invariant CapturedEvidence: captured == 1;
        }
    }
}
