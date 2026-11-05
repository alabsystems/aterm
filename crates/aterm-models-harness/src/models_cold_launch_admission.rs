// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Keeping a saved login requires current authority just as choosing a replacement does.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cold_launch_admission_model() -> Model {
    aterm_spec::ty_model! {
        ColdLaunchAdmission {
            const Buggy = 0;
            var case = 0; var phase = 0; var admitted = 0; var preserved = 1; var rebound = 0;
            action NextCase when (phase == 0 && 14 > case) { case = case + 1; }
            action Decide when (phase == 0) {
                phase = 1;
                admitted = if case == 0 || case == 8 || case == 10 || case == 12 {if Buggy == 2 {0} else {1}}
                    else {if Buggy == 1 {1} else {0}};
                preserved = if Buggy == 3 {0} else {1};
                rebound = if Buggy == 4 {0} else {if case == 10 {1} else {0}};
            }
            invariant CurrentAdmission: phase == 0 || case == 0 || case == 8 || case == 10 || case == 12 || admitted == 0;
            invariant HealthyProgress: phase == 0 || (case > 0 && (8 > case || case == 9 || case == 11 || case > 12)) || admitted == 1;
            invariant RecordsPreserved: preserved == 1;
            invariant CurrentBinding: phase == 0 || 10 > case || case > 10 || rebound == 1;
        }
    }
}
