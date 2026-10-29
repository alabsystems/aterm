// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Claude host selection and final admission reject known target contradictions,
/// preserve rejected evidence, and allow an available alternative to progress.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_host_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountHostIdentity {
            const Buggy = 0;
            var expected = 0; var credential = 0; var default = 0; var backup = 0;
            var seen = 0; var phase = 0; var selected = 0; var next = 0; var hold = 0;
            var started = 0; var rotated = 0; var unchanged = 1; var preserved = 1;
            action Expected when (phase == 0 && expected <= 1) { expected = expected + 1; }
            action Credential when (phase == 0 && credential <= 1) { credential = credential + 1; }
            action Default when (phase == 0 && default == 0) { default = 1; }
            action Backup when (phase == 0 && backup == 0) { backup = 1; }
            action Advice when (phase == 0) {
                seen = credential;
                selected = if Buggy == 1 || expected == 0 || credential == 0 || expected == credential {1}
                    else if backup == 1 {2} else {0};
                next = if expected == 0 || credential == 0 || expected == credential {1}
                    else if backup == 1 {2} else {0};
                phase = if expected == 0 || credential == 0 || expected == credential {1} else {2};
            }
            action Change when (phase == 1 && hold == 0 && credential <= 1) { credential = credential + 1; }
            action Unknown when (phase == 1 && hold == 0 && credential > 0) { credential = 0; }
            action Hold when (phase == 1 && hold <= 1 && ((expected == 1 && credential == 2) || (expected == 2 && credential == 1))) { hold = hold + 1; }
            action Act when (phase == 1) {
                phase = 2;
                started = if Buggy == 2 {1} else if Buggy == 3 {selected}
                    else if Buggy == 4 && credential == 0 {0}
                    else if expected == 0 || credential == 0 || expected == credential {1} else {0};
                rotated = if Buggy == 5 {1} else if Buggy == 6 {0}
                    else if expected == 0 || credential == 0 || expected == credential {1} else {0};
                unchanged = if Buggy == 7 {0}
                    else if expected == 0 || credential == 0 || expected == credential {0} else {1};
                preserved = if Buggy == 8 {0} else {1};
                next = if expected == 0 || credential == 0 || expected == credential {1}
                    else if hold > 0 {if Buggy == 11 && backup == 1 {2} else {0}}
                    else if Buggy == 9 {1} else if Buggy == 10 {0}
                    else if backup == 1 {2} else {0};
            }
            invariant Choice: phase == 0 || selected == if expected == 0 || seen == 0 || expected == seen {1} else if backup == 1 {2} else {0};
            invariant Admission: phase <= 1 || started == if selected == 1 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
            invariant Rotation: rotated == started;
            invariant Records: unchanged == 1 - started;
            invariant Evidence: preserved == 1;
            invariant Progress: phase <= 1 || next == if expected == 0 || credential == 0 || expected == credential {1} else if hold > 0 {0} else if backup == 1 {2} else {0};
            invariant Holding: phase <= 1 || hold == 0 || (started == 0 && next == 0);
        }
    }
}
