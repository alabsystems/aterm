// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code lights' permission-mode return (`aterm-gui`'s `claude_lights`).

use aterm_spec::derive::*;

/// One mode return.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_mode_return_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeModeReturn {
            const Buggy = 0;
            const MaxPresses = 4;
            var mode = 1;
            var start = 1;
            var ring = 1;
            var busy = 0;
            var phase = 0;
            var sent = 0;
            var auto = 0;
            var fresh = 0;
            var covered = 0;
            var presses = 0;
            var named = 0;
            var overshoot = 0;
            var boxed = 0;
            var late = 0;
            var late_press = 0;

            action Click when (phase == 0 && mode > 0 && covered == 0) {
                phase = 1;
                start = mode;
                sent = 1;
                auto = 0;
                fresh = 0;
                presses = 1;
                named = 0;
                late = 0;
            }
            action Answer when (sent == 1 && covered == 0) {
                mode = if mode == 3 && ring == 1 {
                    0
                } else if mode == 3 || mode == 4 {
                    1
                } else {
                    mode + 1
                };
                sent = 0;
                auto = 0;
                fresh = 0;
            }
            action TurnStarts when (busy == 0 && covered == 0) {
                busy = 1;
                fresh = sent;
            }
            action TurnEnds when (busy == 1 && covered == 0) {
                busy = 0;
                fresh = 0;
            }
            action Cover when (covered == 0 && busy == 1 && (sent == 0 || fresh == 0)) {
                covered = 1;
                boxed = if sent == 1 && auto == 1 { 1 } else { boxed };
            }
            action Uncover when (covered == 1) {
                covered = 0;
            }
            action Leave when ((phase == 0 || phase == 2) && mode == 0 && covered == 0) {
                mode = 1;
                phase = 0;
                named = 0;
            }
            action DontAsk when (phase == 0 && mode == 0 && covered == 0) {
                mode = 4;
            }
            action Narrow when (phase == 0 && ring == 1) {
                ring = 0;
            }
            action Timeout when (phase == 1 && late == 0) {
                late = 1;
            }
            action Press when (
                phase == 1
                    && sent == 0
                    && presses <= MaxPresses - 1
                    && (mode > start || start > mode)
                    && (mode > 0 || Buggy == 1)
                    && (covered == 0 || Buggy == 1)
                    && (busy == 0 || Buggy == 1)
                    && (late == 0 || Buggy == 1)
            ) {
                sent = 1;
                auto = 1;
                fresh = 0;
                presses = presses + 1;
                overshoot = if mode == 0 { 1 } else { overshoot };
                boxed = if covered == 1 { 1 } else { boxed };
                late_press = if late == 1 { 1 } else { late_press };
                late = 0;
            }
            action Reject when (
                phase == 1
                    && sent == 0
                    && presses <= MaxPresses - 1
                    && mode > 0
                    && (mode > start || start > mode)
                    && covered == 0
                    && busy == 0
            ) {
                phase = 2;
                named = if Buggy == 1 { 0 } else { 1 };
                late = 0;
            }
            action Lap when (
                phase == 1
                    && sent == 0
                    && mode > 0
                    && (mode == start || presses > MaxPresses - 1)
            ) {
                phase = 2;
                named = if Buggy == 1 { 0 } else { 1 };
                late = 0;
            }
            action Arrive when (phase == 1 && sent == 0 && mode == 0) {
                phase = 2;
                named = 0;
                late = 0;
            }
            action Expire when (phase == 1 && (late == 1 || sent == 1 || covered == 1 || busy == 1)) {
                phase = 2;
                sent = 0;
                auto = 0;
                named = if Buggy == 1 { 0 } else { 1 };
                late = 0;
            }
            action Rest when (phase == 2) {
                phase = 0;
                named = 0;
                late = 0;
            }

            invariant NeverPressesPastExpected: overshoot == 0;
            invariant NoPressIntoABox: boxed == 0;
            invariant NoLatePress: late_press == 0;
            invariant AStopIsNamed: phase <= 1 || mode == 0 || named == 1;
        }
    }
}
