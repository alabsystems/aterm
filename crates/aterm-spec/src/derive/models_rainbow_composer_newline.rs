// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A composer line break starts a new ribbon walk at the Shift+Enter key,
//! but its own typed stamp and a key replayed before the caret's home move
//! cannot spend that walk. The first real echo after the home does.

use super::Model;

/// The bounded key → repaint → first-echo protocol. `Buggy = 1` replays the
/// gap: a chord stamp or type-ahead replay spends the gate before the home,
/// leaving the real new line on the old walk. It also shows how a plain
/// modified key could improperly borrow the composer gate. Tier-1 drives
/// `Ribbon` through these same actions and checks its gate and cohort.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_composer_newline_gate_model() -> Model {
    crate::ty_model! {
        RainbowComposerNewlineGate {
            const Buggy = 0;
            var composer = 0;
            var plain = 0;
            var moved = 0;
            var spent = 0;
            var chord_seen = 0;
            var type_ahead_seen = 0;
            var cancelled = 0;

            action ComposerKey when (composer == 0 && plain == 0) {
                composer = 1;
                moved = 0;
                spent = 0;
                chord_seen = 0;
                type_ahead_seen = 0;
                cancelled = 0;
            }
            action PlainModifiedKey when (composer == 0 && plain == 0) {
                plain = 1;
                composer = if Buggy == 1 { 1 } else { 0 };
            }
            action ChordStamp when (composer == 1 && chord_seen == 0) {
                chord_seen = 1;
                spent = if Buggy == 1 { 1 } else { spent };
            }
            action TypeAheadBeforeHome when (composer == 1 && moved == 0
                && type_ahead_seen == 0) {
                type_ahead_seen = 1;
                spent = if Buggy == 1 { 1 } else { spent };
            }
            action ObserveHome when (composer == 1 && moved == 0) {
                moved = 1;
            }
            action CancelBeforeHome when (composer == 1 && moved == 0) {
                composer = if Buggy == 1 { 1 } else { 0 };
                cancelled = 1;
            }
            action FirstEcho when (composer == 1 && moved == 1 && spent == 0) {
                spent = 1;
            }

            invariant NoGateTheftBeforeHome: spent <= moved;
            invariant PlainKeyCannotArmComposerGate:
                if plain == 1 { composer == 0 && moved == 0 && spent == 0 }
                else { composer <= 1 };
            invariant CancelledGateCannotStrandTyping:
                if cancelled == 1 { composer == 0 && spent == 0 }
                else { composer <= 1 };
        }
    }
}
