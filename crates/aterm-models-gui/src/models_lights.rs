// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code lights' permission-mode toggle (`aterm-gui`'s `claude_lights`).

use aterm_spec::derive::*;

/// One mode toggle: a click on the mode light presses shift+tab until Claude shows the state the
/// light did not: from an expected mode (`mode` 0 bypass, 5 auto) to manual (`mode` 1, `lame`),
/// from any other back to an expected mode (`goal` 1 and 0). A click is taken at rest and while a
/// toggle's end is on show (`phase` 2: its notice or refusal), the click back after an arrival.
///
/// Claude's cycle is the ring `Answer` walks, as `lab footers` measured it on Claude Code 2.1.317
/// (`docs/measured/claude-mode-cycle-2026-11-03.md`): bypass, auto, manual, accept edits, plan and
/// round to bypass in a session started with bypass; auto, manual, accept edits, plan and round to
/// auto in one started without it (`bypass_in` 0). The narrower rings are kept reachable, not
/// measured: one whose cycle has no auto (`auto_in` 0) goes from bypass to manual, and one with
/// neither from plan to manual. From bypass a click on `yolo` presses through auto, an expected
/// mode short of its goal, to manual.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_mode_toggle_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeModeToggle {
            const Buggy = 0;
            const MaxPresses = 4;
            var mode = 1;
            var start = 1;
            var goal = 0;
            var bypass_in = 1;
            var auto_in = 1;
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

            action Click when ((phase == 0 || phase == 2) && covered == 0) {
                phase = 1;
                start = mode;
                goal = if (mode == 0 || mode == 5) && Buggy == 2 {
                    0
                } else if mode == 0 || mode == 5 {
                    1
                } else {
                    0
                };
                sent = 1;
                auto = 0;
                fresh = 0;
                presses = 1;
                named = 0;
                late = 0;
            }
            action Answer when (sent == 1 && covered == 0) {
                mode = if mode == 0 && auto_in == 1 {
                    5
                } else if mode == 0 || mode == 5 || mode == 4 {
                    1
                } else if mode == 3 && bypass_in == 1 {
                    0
                } else if mode == 3 && auto_in == 1 {
                    5
                } else if mode == 3 {
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
            action Leave when (
                (phase == 0 || phase == 2) && ((mode == 0 && auto_in == 0) || mode == 5) && covered == 0
            ) {
                mode = 1;
                phase = 0;
                named = 0;
            }
            action DontAsk when (phase == 0 && mode == 0 && covered == 0) {
                mode = 4;
            }
            action NoBypass when (phase == 0 && bypass_in == 1 && mode > 0) {
                bypass_in = 0;
            }
            action NoAuto when (phase == 0 && auto_in == 1 && 5 > mode) {
                auto_in = 0;
            }
            action Timeout when (phase == 1 && late == 0) {
                late = 1;
            }
            action Press when (
                phase == 1
                    && sent == 0
                    && presses <= MaxPresses - 1
                    && (mode > start || start > mode)
                    && ((goal == 0 && mode > 0 && 5 > mode) || (goal == 1 && (mode > 1 || 1 > mode)) || Buggy == 1)
                    && (covered == 0 || Buggy == 1)
                    && (busy == 0 || Buggy == 1)
                    && (late == 0 || Buggy == 1)
            ) {
                sent = 1;
                auto = 1;
                fresh = 0;
                presses = presses + 1;
                overshoot = if (goal == 0 && (mode == 0 || mode == 5)) || (goal == 1 && mode == 1) {
                    1
                } else {
                    overshoot
                };
                boxed = if covered == 1 { 1 } else { boxed };
                late_press = if late == 1 { 1 } else { late_press };
                late = 0;
            }
            action Reject when (
                phase == 1
                    && sent == 0
                    && presses <= MaxPresses - 1
                    && ((goal == 0 && mode > 0 && 5 > mode) || (goal == 1 && (mode > 1 || 1 > mode)))
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
                    && ((goal == 0 && mode > 0 && 5 > mode) || (goal == 1 && (mode > 1 || 1 > mode)))
                    && (mode == start || presses > MaxPresses - 1)
            ) {
                phase = 2;
                named = if Buggy == 1 { 0 } else { 1 };
                late = 0;
            }
            action Arrive when (
                phase == 1 && sent == 0 && ((goal == 0 && (mode == 0 || mode == 5)) || (goal == 1 && mode == 1))
            ) {
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

            invariant NeverPressesPastItsGoal: overshoot == 0;
            invariant NoPressIntoABox: boxed == 0;
            invariant NoLatePress: late_press == 0;
            invariant AStopIsNamed: phase <= 1
                || (goal == 0 && (mode == 0 || mode == 5))
                || (goal == 1 && mode == 1)
                || named == 1;
            invariant AClickTurnsTheLightOver: phase == 0
                || ((start == 0 || start == 5) && goal == 1)
                || (start > 0 && 5 > start && goal == 0);
        }
    }
}
