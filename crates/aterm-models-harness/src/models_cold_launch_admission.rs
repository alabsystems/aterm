// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Keeping a saved login requires current authority just as choosing a replacement does. In
/// cases 15 and 16 the relaunch's restart asked for another row (`restart account=`): one that
/// holds a fresh reading under its current login takes the relaunch (15), and one that answers no
/// read starts nothing, while the session's own row still serves and is not held down (16).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cold_launch_admission_model() -> Model {
    aterm_spec::ty_model! {
        ColdLaunchAdmission {
            const Buggy = 0;
            var case = 0; var phase = 0; var admitted = 0; var preserved = 1; var rebound = 0;
            action NextCase when (phase == 0 && 16 > case) { case = case + 1; }
            action Decide when (phase == 0) {
                phase = 1;
                admitted = if case == 0 || case == 8 || case == 10 || case == 12 || case > 14 {if Buggy == 2 {0} else {1}}
                    else {if Buggy == 1 {1} else {0}};
                preserved = if Buggy == 3 {0} else {1};
                rebound = if Buggy == 4 {0} else {if case == 10 || case == 15 {1}
                    else {if case == 16 {if Buggy == 5 {1} else {0}} else {0}}};
            }
            invariant CurrentAdmission: phase == 0 || case == 0 || case == 8 || case == 10 || case == 12 || case > 14 || admitted == 0;
            invariant HealthyProgress: phase == 0 || (case > 0 && (8 > case || case == 9 || case == 11 || case == 13 || case == 14)) || admitted == 1;
            invariant RecordsPreserved: preserved == 1;
            invariant CurrentBinding: phase == 0 || 10 > case || case > 10 || rebound == 1;
            invariant AskedAccountTaken: phase == 0 || 15 > case || case > 15 || rebound == 1;
            invariant AskedAccountProven: phase == 0 || 16 > case || rebound == 0;
        }
    }
}
