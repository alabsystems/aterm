// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unsaved Settings draft rides a seamless self-update.

use aterm_spec::derive::Model;

/// One Settings view through one update attempt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_settings_draft_carry_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateSettingsDraftCarry {
            const Buggy = 0;
            const MaxEdits = 2;
            const Surfaced = 9;
            var draft = 0;
            var phase = 0;
            var carried = 0;
            var older = 0;
            var restored = 0;
            var succ = 0;

            action Type when (phase <= 2 && draft <= MaxEdits - 1) {
                draft = draft + 1;
            }
            action Start when (phase == 0) {
                phase = 1;
            }
            action StandDown when (phase == 1) {
                phase = 0;
            }
            action Park when (phase == 1) {
                phase = 2;
                carried = draft;
                older = 0;
            }
            action ParkOlder when (phase == 1 && (draft == 0 || Buggy == 3)) {
                phase = 2;
                carried = draft;
                older = 1;
            }
            action ColdExec when (phase == 1 && (draft == 0 || Buggy == 2)) {
                phase = 3;
                restored = 1;
                succ = 0;
            }
            action Restore when (phase == 2 && restored == 0) {
                restored = 1;
                succ = if older == 1 { 0 } else { carried };
            }
            action RestoreUnreadable when (
                phase == 2 && restored == 0 && older == 0 && carried > 0
            ) {
                restored = 1;
                succ = Surfaced;
            }
            action Commit when (
                phase == 2 && restored == 1 && (carried == draft || Buggy == 1)
            ) {
                phase = 3;
            }
            action Rollback when (phase == 2) {
                phase = 0;
                carried = 0;
                older = 0;
                restored = 0;
                succ = 0;
            }

            invariant NoDraftLost: phase <= 2 || succ == draft || succ == Surfaced;
        }
    }
}
