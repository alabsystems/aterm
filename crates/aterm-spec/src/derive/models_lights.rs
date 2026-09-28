// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code lights' permission-mode return (`aterm-gui`'s
//! `claude_lights`): a click on the mode chip, from a mode the owner does not
//! expect, presses Claude's own shift+tab forward to the nearest mode he
//! does (bypass or auto), each press read back from the ENGINE before the
//! next goes.

use super::*;

/// ONE MODE RETURN, as the host runs it since 2026-09-27. `mode` is Claude's
/// pill in the forward ring the return walks: `0` an expected mode (bypass or
/// auto — both positions of Claude's one permission mode the owner expects),
/// `1` manual, `2` accept edits, `3` plan, `4` don't ask (which leads on to
/// manual); one shift+tab moves it one step on, and from plan it lands on
/// bypass — or, in a session whose cycle holds neither bypass nor auto
/// (`ring == 0`, `Narrow`), back on manual. `start` is the mode the click
/// found. `phase` is the host's: `0` at rest, `1` returning (a click started
/// it — its first press goes AT ONCE, there is no armed or deferred state),
/// `2` ended (a notice or a refusal on show). `sent` is a press Claude has
/// not answered yet, `auto` whether that press is aterm's own (a follow-up)
/// rather than the click's; `busy` a turn in flight; `covered` a box over
/// the composer (a permission prompt).
///
/// The host's actions: `Click` (the person's press on the chip, which sends
/// the first shift+tab), `Press` (each next shift+tab, decided from the
/// engine's reading once Claude has answered the last AND is not mid-turn),
/// `Arrive` (an expected mode shows: done), `Lap` (the cycle came back round,
/// or the presses ran out: stopped, naming the mode), `Reject` (the input
/// seam took no next press: stopped, naming the mode) and `Expire` (a press
/// unanswered, a box up, or a turn still running, past that press's OWN
/// deadline: stopped, naming the mode). Claude's: `Answer` (the pill moves
/// on), `TurnStarts`/`TurnEnds`, `Cover`/`Uncover`; and the person's own
/// mode changes in Claude (`Leave`, `DontAsk`) and a session's narrower
/// cycle (`Narrow`).
///
/// A BOX OPENS ONLY MID-TURN, on a tool call of a turn already running: a
/// turn that starts AFTER a press went (`fresh`) cannot draw one before
/// Claude reads that key — a keystroke is read in milliseconds, a turn's
/// first tool call takes seconds. That is the timing the model assumes, and
/// why the host sends its own presses only while Claude is idle: a read
/// that shows no box cannot see one opening mid-turn between the read and
/// Claude reading the key, and a shift+tab that lands in a box is the box's
/// ANSWER (the 2.1.283 edit prompt binds it to "yes, allow edits for this
/// session"). The click's own press is the person's, like their own
/// shift+tab in Claude.
///
/// `MaxPresses` bounds a return's presses as the host's lap check and
/// `lights::MAX_MODE_PRESSES` do; from don't ask the nearest expected mode
/// is four away.
///
/// The laws: no press is ever sent from an expected mode — the owner's
/// complaint of 2026-09-27 was a click on "auto-approve" that pressed auto
/// mode away (`NeverPressesPastExpected`); none of aterm's own presses goes
/// into a box, where a shift+tab would be the box's answer
/// (`NoPressIntoABox`); and a return that ends anywhere but an expected mode
/// says where it stopped (`AStopIsNamed`), never the old generic "Claude did
/// not switch". `Buggy = 1` replays each defect: a press decided without
/// reading the mode (the retired "any mode but this one" toggle), a press
/// into a box or mid-turn, and a stop — by deadline, lap or rejected press
/// — that drops the stopped mode's name.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_mode_return_model() -> Model {
    crate::ty_model! {
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

            action Click when (phase == 0 && mode > 0 && covered == 0) {
                phase = 1;
                start = mode;
                sent = 1;
                auto = 0;
                fresh = 0;
                presses = 1;
                named = 0;
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
            action Press when (
                phase == 1
                    && sent == 0
                    && presses <= MaxPresses - 1
                    && (mode > start || start > mode)
                    && (mode > 0 || Buggy == 1)
                    && (covered == 0 || Buggy == 1)
                    && (busy == 0 || Buggy == 1)
            ) {
                sent = 1;
                auto = 1;
                fresh = 0;
                presses = presses + 1;
                overshoot = if mode == 0 { 1 } else { overshoot };
                boxed = if covered == 1 { 1 } else { boxed };
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
            }
            action Lap when (
                phase == 1
                    && sent == 0
                    && mode > 0
                    && (mode == start || presses > MaxPresses - 1)
            ) {
                phase = 2;
                named = if Buggy == 1 { 0 } else { 1 };
            }
            action Arrive when (phase == 1 && sent == 0 && mode == 0) {
                phase = 2;
                named = 0;
            }
            action Expire when (phase == 1 && (sent == 1 || covered == 1 || busy == 1)) {
                phase = 2;
                sent = 0;
                auto = 0;
                named = if Buggy == 1 { 0 } else { 1 };
            }
            action Rest when (phase == 2) {
                phase = 0;
                named = 0;
            }

            invariant NeverPressesPastExpected: overshoot == 0;
            invariant NoPressIntoABox: boxed == 0;
            invariant AStopIsNamed: phase <= 1 || mode == 0 || named == 1;
        }
    }
}
