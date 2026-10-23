// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage tap's record across another account signing in to the home.

use aterm_spec::derive::Model;

/// THE ACCOUNT A USAGE TAP'S RECORD HOLDS (the account-stats design §3.1, §9 A1 as built).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_account_key_model() -> Model {
    aterm_spec::ty_model! {
        HarnessAccountKey {
            const Buggy = 0;
            var home = 0;
            var on = 0;
            var adopt = 0;
            var key = 0;
            var held = 0;
            var gate = 0;
            var file = 0;
            var early = 0;

            action SignIn when (home == 0) {
                home = 1;
                adopt = 1;
            }
            action Adopt when (adopt == 1) {
                on = home;
                adopt = 0;
            }
            action Elapse when (gate == 1 && adopt == 0) {
                gate = 2;
            }
            action Reply {
                file = on + 1;
                early = if (gate == 2) { 0 } else { 1 };
            }
            action Read when (file > 0) {
                held = if (key == home) {
                    if (early == 1 && gate > 0) { held } else { file - 1 }
                } else if (early == 1) {
                    2
                } else {
                    file - 1
                };
                gate = if (key == home) { gate } else { 1 };
                key = home;
            }

            action ReadUngated when (Buggy == 1 && file > 0) {
                held = file - 1;
                gate = if (key == home) { gate } else { 1 };
                key = home;
            }
            action Restamp when (Buggy == 1 && file > 0 && gate == 2 && early == 1) {
                early = 0;
            }

            invariant TheFigureIsTheKeysAccount: held == 2 || held == key;
        }
    }
}
