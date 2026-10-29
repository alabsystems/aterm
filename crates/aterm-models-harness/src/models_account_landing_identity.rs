// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Completion keeps contradictory target identities pending until corrected.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_landing_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountLandingIdentity {
            const Buggy = 0;
            var expected = 0; var before = 0; var credential = 0; var denied = 0;
            var default = 0; var mode = 0; var phase = 0; var polls = 0;
            var pending = 1; var written = 0; var moved = 0; var notified = 0;
            var preserved = 1; var struck = 0; var quiet = 1;
            action Expected when (phase == 0 && expected <= 1) { expected = expected + 1; }
            action Before when (phase == 0 && before <= 1) { before = before + 1; }
            action Credential when (phase == 0 && credential <= 1) { credential = credential + 1; }
            action Default when (phase == 0 && default == 0) { default = 1; }
            action Mode when (phase == 0 && mode <= 1) { mode = mode + 1; }
            action Complete when (phase == 0) {
                phase = 1;
                denied = credential;
                written = if Buggy == 1 {if expected == 0 || before == 0 || expected == before {1} else {0}}
                    else if Buggy == 2 || (Buggy == 4 && default == 1) {1}
                    else if Buggy == 3 && expected > 0 && credential == 0 {0}
                    else if expected == 0 || credential == 0 || expected == credential {1} else {0};
                pending = if Buggy == 5 {0}
                    else if expected == 0 || credential == 0 || expected == credential {0} else {1};
                moved = if Buggy == 6 {1}
                    else if expected == 0 || credential == 0 || expected == credential {1} else {0};
                notified = if Buggy == 7 {1}
                    else if mode == 1 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                preserved = if Buggy == 8 {0} else {1};
                struck = if Buggy == 9 {1} else {0};
                quiet = if Buggy == 14 {0}
                    else if expected == 0 || credential == 0 || expected == credential {0} else {1};
            }
            action Poll when (phase == 1 && pending == 1 && polls == 0) {
                polls = 1;
                written = if Buggy == 15 {1} else {written};
                quiet = if Buggy == 15 {0} else {1};
            }
            action Correct when (phase == 1 && pending == 1) {
                credential = expected;
                phase = 2;
            }
            action Retry when (phase == 2) {
                phase = 3;
                pending = if Buggy == 12 {1} else {0};
                written = if Buggy == 12 {0} else {1};
                moved = if Buggy == 12 {0} else {1};
                notified = if Buggy == 13 {0} else if mode == 1 {1} else {0};
                quiet = 0;
            }
            action Replay when ((phase == 1 || phase == 3) && pending == 0) {
                phase = 4;
                written = if Buggy == 10 {written + 1} else {written};
                notified = if Buggy == 11 {notified + 1} else {notified};
                quiet = 1;
            }
            invariant Admission: phase == 0 || phase > 1 || written == if expected == 0 || credential == 0 || expected == credential {1} else {0};
            invariant Pending: pending + written == 1;
            invariant Publication: moved == written;
            invariant Notification: notified == if written > 0 && mode == 1 {1} else {0};
            invariant Evidence: preserved == 1 && struck == 0;
            invariant Records: phase == 0 || phase > 1 || written == 1 || quiet == 1;
            invariant Progress: phase <= 2 || written == 1;
            invariant Once: written <= 1 && notified <= 1;
        }
    }
}
