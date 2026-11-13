// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code lights' permission click (`aterm-gui`'s `permission_click` and
//! `claude_lights`).

use aterm_spec::derive::*;

/// One click on the permission chip (`docs/DESIGN-yolo-binary-2026-11-04.md` §2, §3): it types
/// nothing, but stands an ask in the window's memory to restart the tab's conversation on the
/// other line (`bit` 1 yolo on, 0 off), fixed at the click (`from`, `to`): from off onto the
/// bypass line, from on onto the line with no permission flag, which lands in the settings'
/// default mode. Where that default is itself yolo (`home` 1) a click on the on chip would
/// restart into the mode it left, and where the settings turn bypass off (`nobypass` 1) a click
/// on the off chip would ask a line Claude Code does not start in: either stands nothing and only
/// says so (`Explain`).
/// The ask's lane (`lane` 1) looks at the tab at its pace until it may signal the restart (`lane`
/// 2), which lands once and ends the ask, wherever the pill then reads. Before the signal a click on
/// the standing ask takes it back; after it nothing can. A refusal ends it and is said; the tab
/// closing or its conversation leaving it ends it quietly.
///
/// The environment: the pill moves by Claude's own steps (`ToYolo`, `ToOff`); the settings'
/// default and their bar on bypass move by an edit of a settings file (`DefaultYolo`,
/// `DefaultElse`, `BypassBarred`, `BypassAllowed`); a box or a person's
/// hold makes the lane wait (`held`); a tab where no line can be made (`never`: a job with no
/// shell, a claim, a coordinated record) can only be refused; a run of presses within a double
/// click's interval (`run`) is one click until it ends (`Apart`); a refusal on show rests.
///
/// Not modelled: the lane's re-key to a conversation its own restart afresh started (`Moved`), and
/// the take-back that races the signal, which the harness fences with no I/O
/// (`relaunch::restart_fenced`) and the window tests directly.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_permission_click_model() -> Model {
    aterm_spec::ty_model! {
        ClaudePermissionClick {
            const Buggy = 0;
            var bit = 0;
            var from = 0;
            var to = 1;
            var stands = 0;
            var lane = 0;
            var conv = 1;
            var asked = 1;
            var tab = 1;
            var held = 0;
            var never = 0;
            var run = 0;
            var ended = 0;
            var said = 0;
            var typed = 0;
            var again = 0;
            var into_held = 0;
            var into_never = 0;
            var home = 0;
            var idle = 0;
            var nobypass = 0;
            var barred = 0;

            action Click when (
                stands == 0
                    && lane == 0
                    && tab == 1
                    && ((bit == 0 && (nobypass == 0 || Buggy == 11))
                        || (bit == 1 && (home == 0 || Buggy == 10)))
                    && (run == 0 || Buggy == 3)
            ) {
                stands = 1;
                lane = 1;
                from = bit;
                to = if Buggy == 1 || bit == 0 { 1 } else { 0 };
                asked = conv;
                run = 1;
                ended = 0;
                said = 0;
                typed = 0;
                again = if run == 1 { 1 } else { again };
                idle = if bit == 1 && home == 1 { 1 } else { idle };
                barred = if bit == 0 && nobypass == 1 { 1 } else { barred };
            }
            action Explain when (
                stands == 0
                    && lane == 0
                    && tab == 1
                    && run == 0
                    && ((bit == 1 && home == 1) || (bit == 0 && nobypass == 1))
            ) {
                run = 1;
            }
            action TakeBack when (stands == 1 && (lane == 1 || Buggy == 2) && (run == 0 || Buggy == 3)) {
                stands = 0;
                lane = if Buggy == 2 { lane } else { 0 };
                run = 1;
                again = if run == 1 { 1 } else { again };
            }
            action Refuse when (lane == 1 && stands == 1 && never == 1) {
                stands = 0;
                lane = 0;
                ended = 1;
                said = if Buggy == 4 { 0 } else { 1 };
            }
            action Signal when (
                lane == 1
                    && stands == 1
                    && tab == 1
                    && conv == asked
                    && typed <= 1
                    && (held == 0 || Buggy == 5)
                    && (never == 0 || Buggy == 6)
            ) {
                lane = 2;
                typed = typed + 1;
                into_held = if held == 1 { 1 } else { into_held };
                into_never = if never == 1 { 1 } else { into_never };
            }
            action Land when (lane == 2) {
                lane = if Buggy == 7 && bit == from { 1 } else { 0 };
                stands = if Buggy == 7 && bit == from { 1 } else { 0 };
            }
            action CloseTab when (tab == 1) {
                tab = 0;
                stands = if Buggy == 8 { stands } else { 0 };
                lane = if Buggy == 8 { lane } else { 0 };
            }
            action Leave when (conv == 1 && lane <= 1 && tab == 1) {
                conv = 2;
                stands = if Buggy == 9 { stands } else { 0 };
                lane = if Buggy == 9 { lane } else { 0 };
            }
            action ToYolo when (bit == 0) {
                bit = 1;
            }
            action ToOff when (bit == 1) {
                bit = 0;
            }
            action DefaultYolo when (home == 0) {
                home = 1;
            }
            action DefaultElse when (home == 1) {
                home = 0;
            }
            action BypassBarred when (nobypass == 0) {
                nobypass = 1;
            }
            action BypassAllowed when (nobypass == 1) {
                nobypass = 0;
            }
            action Hold when (held == 0) {
                held = 1;
            }
            action Free when (held == 1) {
                held = 0;
            }
            action NoLine when (never == 0) {
                never = 1;
            }
            action Apart when (run == 1) {
                run = 0;
            }
            action Rest when (ended == 1) {
                ended = 0;
                said = 0;
            }

            invariant AClickFlipsTheBit: stands == 0 || from + to == 1;
            invariant TheSignalKeepsItsAsk: lane <= 1 || stands == 1;
            invariant ADoubleClickIsOneClick: again == 0;
            invariant ARefusalIsSaid: ended == 0 || said == 1;
            invariant NoSignalIntoAWait: into_held == 0;
            invariant NoLineWhereNoneCanBeMade: into_never == 0;
            invariant OneRestartPerClick: typed <= 1;
            invariant NoAskOutlivesItsTab: tab == 1 || stands == 0;
            invariant NoAskOutlivesItsConversation: stands == 0 || conv == asked;
            invariant NoRestartIntoTheModeItLeft: idle == 0;
            invariant NoBypassLineWhereBypassIsOff: barred == 0;
        }
    }
}
