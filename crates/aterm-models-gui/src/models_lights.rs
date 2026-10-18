// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code lights' permission-mode return (`aterm-gui`'s `claude_lights`): a click on
//! the mode chip, from a mode the owner does not expect, presses Claude's own shift+tab forward
//! to the nearest one they do (bypass or auto), reading each press back before the next.

use aterm_spec::derive::*;

/// One mode return. `mode` is Claude's pill in the forward ring the return walks: `0` an
/// expected mode (bypass or auto), `1` manual, `2` accept edits, `3` plan, `4` don't ask (which
/// leads on to manual); one shift+tab moves it one step, and from plan it lands on bypass, or on
/// manual in a session whose cycle holds neither (`ring == 0`, `Narrow`). `start` is the mode
/// the click found. `phase` is the host's: `0` at rest, `1` returning (the click's press goes at
/// once; there is no armed state), `2` ended (a notice or refusal on show). `sent` is a press
/// Claude has not answered, `auto` whether it is aterm's own rather than the click's; `busy` a
/// turn in flight; `covered` a box over the composer. `late` says the press's deadline passed
/// before the loop handled Claude's answer: the loop may read the answer but not press from it.
///
/// Host actions: `Click` (sends the first shift+tab), `Press` (each next one, once Claude has
/// answered the last and is not mid-turn), `Arrive` (an expected mode shows), `Lap` (the cycle
/// came back round or the presses ran out), `Reject` (the input seam took no press), `Expire`
/// (a press unanswered, a box up or a turn running past that press's deadline), and `Timeout`
/// (a late loop turn). Claude's: `Answer`, `TurnStarts`/`TurnEnds`, `Cover`/`Uncover`; the
/// person's: `Leave`, `DontAsk`, and a narrower cycle (`Narrow`).
///
/// A box opens only mid-turn, on a tool call: a turn that starts after a press went (`fresh`)
/// cannot draw one before Claude reads the key (milliseconds against a tool call's seconds).
/// So the host presses only while Claude is idle: a read that shows no box cannot see one
/// opening mid-turn, and a shift+tab into a box answers it (Claude Code 2.1.283's edit prompt
/// takes it as "yes, allow edits for this session"). The click's own press is the person's.
///
/// `MaxPresses` bounds a return's presses as the host's lap check and `lights::MAX_MODE_PRESSES`
/// do; from don't ask the nearest expected mode is four presses away.
///
/// Laws: no press from an expected mode (`NeverPressesPastExpected`); none of aterm's presses
/// into a box (`NoPressIntoABox`); a return that ends off an expected mode names where it
/// stopped (`AStopIsNamed`); no press after its deadline from a delayed wake (`NoLatePress`).
/// `Buggy = 1` replays each: a press decided without reading the mode, a press into a box or
/// mid-turn or late, and a stop that drops the mode's name.
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
