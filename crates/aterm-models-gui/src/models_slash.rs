// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the footer facts' slash press (`aterm-gui`'s `claude_slash`).

use aterm_spec::derive::*;

/// Formal model for one Claude Code slash-command press from the footer facts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_slash_press_model() -> Model {
    aterm_spec::ty_model! {
        ClaudeSlashPress {
            const Buggy = 0;
            var phase = 0;
            var composer = 0;
            var echoed = 0;
            var vim = 0;
            var boxed = 0;
            var busy = 0;
            var person = 0;
            var quiet = 0;
            var named = 0;
            var writes = 0;
            var over_draft = 0;
            var into_box = 0;
            var while_busy = 0;
            var garbage = 0;
            var unechoed = 0;
            var after_person = 0;

            action Click when (
                phase == 0
                    && (composer == 0 || Buggy == 1)
                    && (boxed == 0 || Buggy == 2)
                    && (busy == 0 || Buggy == 3)
            ) {
                phase = 1;
                writes = 1;
                echoed = 0;
                quiet = 0;
                person = 0;
                named = 0;
                over_draft = if composer == 2 { 1 } else { over_draft };
                into_box = if boxed == 1 { 1 } else { into_box };
                while_busy = if busy == 1 { 1 } else { while_busy };
            }
            action Echo when (
                (phase == 1 || phase == 2)
                    && echoed == 0
                    && vim == 0
                    && boxed == 0
                    && composer <= 1
            ) {
                composer = 1;
                echoed = 1;
            }
            action Type when (
                phase == 1
                    && (echoed == 1 || Buggy == 4)
                    && person == 0
                    && boxed == 0
                    && busy == 0
            ) {
                phase = 2;
                writes = writes + 1;
                echoed = 0;
                quiet = 0;
                garbage = if vim == 1 { 1 } else { garbage };
                composer = if vim == 1 { 2 } else { composer };
            }
            action Settle when (phase == 2 && echoed == 1 && quiet == 0) {
                quiet = 1;
            }
            action Enter when (
                phase == 2
                    && ((echoed == 1 && quiet == 1 && composer == 1) || Buggy == 5 || Buggy == 6)
                    && (person == 0 || Buggy == 6)
                    && boxed == 0
                    && busy == 0
            ) {
                phase = 3;
                writes = writes + 1;
                unechoed = if echoed == 1 && composer == 1 { unechoed } else { 1 };
                after_person = if person == 1 { 1 } else { after_person };
            }
            action Done when (phase == 3 && person == 0) {
                phase = 4;
                composer = 0;
                echoed = 0;
                named = 1;
            }
            action Stop when (phase > 0 && phase <= 3) {
                phase = 4;
                named = if Buggy == 7 { 0 } else { 1 };
            }
            action Rest when (phase == 4) {
                phase = 0;
                named = 0;
                quiet = 0;
                echoed = 0;
                person = 0;
            }
            action PersonTypes when (composer <= 1) {
                composer = 2;
                echoed = 0;
                person = if phase > 0 && phase <= 3 { 1 } else { person };
            }
            action PersonMoves when (phase > 0 && phase <= 3 && person == 0) {
                person = 1;
            }
            action Clears when (phase == 0 && composer > 0) {
                composer = 0;
            }
            action VimNormal when (phase == 0 && vim == 0) {
                vim = 1;
            }
            action BoxOpens when (boxed == 0) {
                boxed = 1;
            }
            action BoxCloses when (boxed == 1) {
                boxed = 0;
            }
            action TurnStarts when (busy == 0) {
                busy = 1;
            }
            action TurnEnds when (busy == 1) {
                busy = 0;
            }

            invariant NoTypeOverDraft: over_draft == 0;
            invariant NoTypeIntoBox: into_box == 0;
            invariant NoTypeWhileBusy: while_busy == 0;
            invariant NoMotionsInVim: garbage == 0;
            invariant NoEnterUnlessOurEcho: unechoed == 0;
            invariant NoEnterAfterPerson: after_person == 0;
            invariant AStopIsNamed: phase <= 3 || named == 1;
            bound Writes: writes <= 3;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::claude_slash_press_model;

    /// Each law holds with no mutant on, and its own break is caught: the
    /// k-th `Buggy` dial breaks the k-th law (a ghost law would pass its
    /// break too).
    #[test]
    fn each_law_holds_and_its_own_break_is_caught() {
        let model = claude_slash_press_model();
        assert!(
            aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_buggy(&model, 0))
                .is_empty()
        );
        for (dial, law) in [
            (1, "NoTypeOverDraft"),
            (2, "NoTypeIntoBox"),
            (3, "NoTypeWhileBusy"),
            (4, "NoMotionsInVim"),
            (5, "NoEnterUnlessOurEcho"),
            (6, "NoEnterAfterPerson"),
            (7, "AStopIsNamed"),
        ] {
            let broken = aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_buggy(
                &model, dial,
            ));
            assert!(
                broken.contains(&law),
                "Buggy = {dial} breaks {law}: {broken:?}"
            );
        }
    }

    /// The measured vim NORMAL trace (2.1.304): the probe draws nothing, so
    /// the press can only stop, named — and the retired whole-command write
    /// (the rest typed with no read-back) leaves motions' garbage.
    #[test]
    fn in_vim_normal_the_probe_only_stops() {
        let model = claude_slash_press_model();
        let mut state = model.init_state();
        for action in ["VimNormal", "Click"] {
            assert!(model.fire(action, &mut state), "{action}");
        }
        assert!(
            model.successors("Echo", &state).is_empty(),
            "nothing echoes"
        );
        assert!(model.successors("Type", &state).is_empty(), "no rest typed");
        assert!(model.fire("Stop", &mut state));
        assert_eq!(state["named"], 1);
        assert_eq!(state["composer"], 0, "the composer as it was");
        let buggy = aterm_spec::interp::with_buggy(&model, 4);
        let mut raced = buggy.init_state();
        for action in ["VimNormal", "Click", "Type"] {
            assert!(buggy.fire(action, &mut raced), "{action}");
        }
        assert!(!buggy.check_invariant("NoMotionsInVim", &raced));
    }
}
