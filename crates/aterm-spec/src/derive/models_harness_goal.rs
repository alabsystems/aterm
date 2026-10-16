// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live upgrade's goal pause. A Codex in goal mode starts its next turn within about 14 ms of
//! the last one's end, so it never gives aterm a pause to upgrade in. Once the upgrade is due,
//! aterm types `/goal pause` (or presses Esc the instant a new goal turn starts, before it has
//! done anything), moves Codex onto the new build, then types `/goal resume`; the goal carries
//! on and no running tool call is cut off.
//!
//! A sibling of `HarnessUpgradeLadder` (`models_harness.rs`): the ladder states the move's floors
//! at every rung, this states the goal's. Tier-1 is aterm-agent's
//! `harness/upgrade_goal_tests.rs`, binding `upgrade_codex::goal_step`, `goal_owed` and
//! `daemon_turn`, with the clock aged through the lane's real readers
//! (`upgrade_drive::St::rung_at`, `upgrade_codex::goal_hold_past_bound`,
//! `upgrade_codex::goal_rest_until`) over every reachable state.

use super::*;

/// The goal pause: its lane and its world, one Codex tab whose conversation pursues a goal.
///
/// The world: the goal's turns (`TurnEnds` starts the next at its head at once while the goal is
/// pursued, so the lane never looks between two, and none once it is paused; `TurnWorks` takes
/// a head past it, where a tool may run; a goal resumed on an idle thread begins its continuation
/// at once, as Codex's `continue_if_idle` does), Codex taking a typed pause (`PauseTakes`), a
/// person resuming or pausing the goal (`PersonResumes`, `PersonPauses`; `person` is their hand
/// since the lane's last edge), a save-then-wait switch opening and closing, the ladder's clock
/// (`Advance`: the Land rung or the owner's `--now`, as `St::rung_at` reads it), the move not
/// coming within the hold's bound (`MoveOverdue`: another ladder floor standing for an hour), and
/// the rest after a hold that ended without its move (`Rested`).
///
/// The lane is `upgrade_codex::goal_step` and its record `goal_hold`: `held` 0 none (or a hold
/// rested), 1 pausing (typed, not seen), 2 paused (seen, owes the goal its resume), 3 resumed,
/// 4 released to a person's hand.
///
/// * `GoalPause` — the goal pursued, its own turn holding the move, at the Land rung, no switch
///   open: `/goal pause`, which Codex takes while a turn runs and which stops the next turn,
///   never the running one (`turn` untouched);
/// * `GoalEsc` — a typed pause not taken, the goal's turn at its head (`turn` 1: its rollout
///   holds none of its own work), nobody's hand, no switch: the Esc, which stops that turn and
///   pauses the goal at once;
/// * `Took` — the pause seen on the footer with nobody's hand on the tab since it was typed;
///   `Release` — the goal pursued again after it was seen paused, or a person's hand on a pause
///   not seen yet (what shows may be their own Esc or `/goal pause`): no longer the lane's to
///   resume;
/// * `GoalResume` — the goal it holds paused, once the move is made or will not come, no switch
///   open: `/goal resume` (or the relaunched Codex's box's `Resume goal`);
/// * `LetGo` — the host lets the tab go (`upgrade_drive::due` says no more): only with no resume
///   owed (`upgrade_codex::goal_owed`).
///
/// `LadderMove` is the ladder's (bound in `HarnessUpgradeLadder`, `upgrade::gate` and
/// `upgrade_codex::daemon_turn`) as that rule sees this tab: no turn of its own running, and the
/// goal not pursued, whoever paused it.
///
/// At the paused goal's idle points the supervisor is the host's: every wait the lane words at a
/// held goal is `goal-held`, which owns the session's turn ends (`goal_wait_word`,
/// `upgrade_drive::owns_turn_ends`), so nothing continues it. The mutant `ContinuePaused` is the
/// supervisor typing a turn onto the paused thread at each such point.
///
/// Safety, each invariant caught by its own `Buggy` action alone
/// (`verify::audit_dead_negative_controls`): `NeverPausedPastMove` (`LetGoPaused`: the host lets
/// a current tab go with its goal still paused by aterm), `NoToolCutOff` (`EscMidWork`: the Esc
/// pressed past a turn's head), `OneResume` (`ResumeTwice`: a second resume over a goal the person
/// paused after aterm's), `NeverOverAPerson` (`ResumeOverPerson`: the goal resumed after the
/// person took it), `NotUnderASwitch` (`PauseUnderSwitch`), `OnlyAtLand` (`PauseAtFirstSight`: the
/// pause made before the ladder's last rung), `TheGoalIsPausedAtLand` (`GoalHoldsTheTab`: a goal
/// holds its tab like any turn; also a livelock of the liveness) and `NothingTypedOverThePause`
/// (`ContinuePaused`; also a livelock).
///
/// Not modelled: the lane's give-up of a pause that never shows (`GoalStep::GiveUp`,
/// `PAUSE_GIVE_UP_S`; it types nothing, bound by its unit tests), the resume's own retry
/// (`Resuming`, bound by the drive's tests), a person who resumes and pauses the goal again
/// between two looks of the lane (the footer reads paused at both, and the lane resumes it after
/// the move), and a person's hand after the pause was seen that leaves the goal as it is (a
/// message they type, their Esc into the paused goal's last turn): the goal stays the lane's to
/// resume. `NeverOverAPerson` covers a person who took the goal: resumed it, or paused it before
/// the lane saw its own pause.
///
/// Liveness: [`harness_upgrade_goal_pause_liveness`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_goal_pause_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeGoalPause {
            const Buggy = 0;
            var behind = 1;
            var land = 0;
            var goal = 1;
            var turn = 2;
            var held = 0;
            var overdue = 0;
            var switch = 0;
            var person = 0;
            var looked = 1;
            var pauses = 1;
            var resumes = 0;
            var at_work = 0;
            var at_switch = 0;
            var early = 0;
            var over = 0;
            var slipped = 0;
            var typed = 0;

            // The ladder's clock: two hours behind, or the owner's `--now`.
            action Advance when (land == 0) {
                land = 1;
            }
            // A goal turn ends; while the goal is pursued the next begins at
            // its head at once (no look between), once paused none does.
            action TurnEnds when (turn > 0) {
                turn = if goal == 1 { 1 } else { 0 };
                typed = 0;
            }
            // The turn's head passed: its first work lands (a tool may run).
            action TurnWorks when (turn == 1) {
                turn = 2;
            }
            // Codex takes the typed `/goal pause`.
            action PauseTakes when (held == 1 && goal == 1) {
                goal = 2;
            }
            // A person's hand on the goal. A goal resumed on an idle thread
            // begins its continuation at once (`continue_if_idle`).
            action PersonResumes when (goal == 2) {
                goal = 1;
                person = 1;
                turn = if turn == 0 { 1 } else { turn };
            }
            action PersonPauses when (goal == 1) {
                goal = 2;
                person = 1;
            }
            action SwitchOpens when (switch == 0) {
                switch = 1;
            }
            action SwitchCloses when (switch == 1) {
                switch = 0;
            }
            // Another floor of the ladder's held the move past the hold's bound.
            action MoveOverdue when (held == 2 && behind == 1 && overdue == 0) {
                overdue = 1;
            }
            // The rest after a hold that ended without its move.
            action Rested when (held > 2 && behind == 1) {
                held = 0;
                overdue = 0;
                person = 0;
                resumes = 0;
            }

            // The lane.
            action GoalPause when (
                looked == 1 && pauses == 1 && behind == 1 && land == 1 &&
                goal == 1 && turn > 0 && held == 0 && switch == 0
            ) {
                held = 1;
                person = 0;
            }
            action GoalEsc when (
                looked == 1 && held == 1 && goal == 1 && turn == 1 &&
                person == 0 && switch == 0
            ) {
                goal = 2;
                turn = 0;
                at_work = turn;
            }
            // The pause seen — never with a person's hand on the tab since it
            // was typed: their Esc, or their `/goal pause`, may be what shows.
            action Took when (looked == 1 && held == 1 && goal == 2 && person == 0) {
                held = 2;
            }
            action Release when (
                looked == 1 && ((goal == 1 && held == 2) || (held == 1 && person == 1))
            ) {
                held = 4;
            }
            // The ladder's move, as it sees this tab.
            action LadderMove when (looked == 1 && behind == 1 && turn == 0 && goal == 2) {
                behind = 0;
            }
            action GoalResume when (
                looked == 1 && held == 2 && goal == 2 && switch == 0 &&
                (behind == 0 || overdue == 1)
            ) {
                goal = 1;
                held = 3;
                resumes = resumes + 1;
                person = 0;
                turn = if turn == 0 { 1 } else { turn };
            }
            action LetGo when (looked == 1 && behind == 0 && (held == 0 || held > 2)) {
                looked = 0;
            }

            // The mutants, each its own dead action, each from a state no other has touched.
            // A goal holds its tab like any turn.
            action GoalHoldsTheTab when (
                Buggy == 1 && slipped == 0 && pauses == 1 && behind == 1 && land == 1 &&
                goal == 1 && turn > 0 && held == 0 && switch == 0
            ) {
                slipped = 1;
                pauses = 0;
            }
            // The Esc past a turn's head: a tool call cut off.
            action EscMidWork when (
                Buggy == 1 && slipped == 0 && held == 1 && goal == 1 && turn == 2 &&
                person == 0 && switch == 0
            ) {
                slipped = 1;
                goal = 2;
                turn = 0;
                at_work = turn;
            }
            action PauseUnderSwitch when (
                Buggy == 1 && slipped == 0 && switch == 1 && behind == 1 && land == 1 &&
                goal == 1 && turn > 0 && held == 0
            ) {
                slipped = 1;
                held = 1;
                at_switch = 1;
            }
            action PauseAtFirstSight when (
                Buggy == 1 && slipped == 0 && land == 0 && behind == 1 &&
                goal == 1 && turn > 0 && held == 0 && switch == 0
            ) {
                slipped = 1;
                held = 1;
                early = 1;
            }
            // A resume made again over a goal the person paused after aterm's.
            action ResumeTwice when (Buggy == 1 && slipped == 0 && held == 3 && goal == 2) {
                slipped = 1;
                goal = 1;
                resumes = resumes + 1;
            }
            // The goal resumed after a person took it from aterm.
            action ResumeOverPerson when (Buggy == 1 && slipped == 0 && held == 4 && goal == 2) {
                slipped = 1;
                goal = 1;
                over = 1;
            }
            // The host letting a current tab go with its goal left paused.
            action LetGoPaused when (
                Buggy == 1 && slipped == 0 && looked == 1 && behind == 0 && held == 2
            ) {
                slipped = 1;
                looked = 0;
            }
            // The supervisor typing a continuation onto the paused thread at every idle point
            // the host's words left unowned: the thread stays busy and the move never comes.
            action ContinuePaused when (
                Buggy == 1 && looked == 1 && held == 2 && goal == 2 && turn == 0
            ) {
                turn = 1;
                typed = 1;
            }

            invariant NeverPausedPastMove: looked == 1 || held == 0 || held > 2;
            invariant NoToolCutOff: at_work <= 1;
            invariant OneResume: resumes <= 1;
            invariant NeverOverAPerson: over == 0;
            invariant NotUnderASwitch: at_switch == 0;
            invariant OnlyAtLand: early == 0;
            invariant TheGoalIsPausedAtLand: pauses == 1;
            invariant NothingTypedOverThePause: typed == 0;
        }
    }
}

/// The upgrade lands under a goal that never pauses by itself, and leaves it pursued:
/// `[]<>(moved-and-resumed \/ a-floor-stands)`. Again and again the move is made with no resume
/// owed (`behind = 0 /\ held ∉ {1,2}`), or a floor the lane never passes stands: the paused
/// goal's own last turn still running (`turn > 0 /\ goal = 2 /\ typed = 0`; a turn typed onto
/// the paused thread is no such floor), a switch open, a person's hand on the goal since the
/// lane's last edge, or another ladder floor holding the move past the hold's bound (`overdue`).
/// A goal pursued with its turns chained is no floor.
///
/// The environment is not fair: turns, a person, the switch and the ladder's other floors may
/// go on for ever. The verdict assumes only:
///
/// * `Advance` — the wall clock moves (weakly fair);
/// * `GoalPause`, `Took`, `LadderMove`, `GoalResume` — the lane keeps looking at the tab's
///   idle-looking points (a goal-mode tab shows one at every goal turn's start) and acts where
///   its rule is open (weakly fair);
/// * `PauseTakes` — Codex takes a typed `/goal pause` (weakly fair): it dispatches `/goal` at
///   once while a turn runs (`upgrade_codex` module doc, "The goal pause"). The Esc at a turn's
///   head is the fallback for a build that would not, outside this claim.
///
/// The mutants that break it, each alone: `GoalHoldsTheTab` (a goal holds its tab for as long as
/// it runs, which is for ever), `LetGoPaused` (the host lets the current tab go with its goal
/// left paused) and `ContinuePaused` (the supervisor types a turn onto the paused thread at each
/// idle point, so the move never finds it idle).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::harness_upgrade_goal_pause_model)]
pub fn harness_upgrade_goal_pause_liveness() -> Liveness {
    Liveness {
        name: "TheUpgradeLandsUnderAGoal",
        goal: or_(
            and_(
                eq(var("behind"), int(0)),
                or_(eq(var("held"), int(0)), gt(var("held"), int(2))),
            ),
            or_(
                or_(
                    and_(
                        and_(gt(var("turn"), int(0)), eq(var("goal"), int(2))),
                        eq(var("typed"), int(0)),
                    ),
                    eq(var("switch"), int(1)),
                ),
                or_(eq(var("person"), int(1)), eq(var("overdue"), int(1))),
            ),
        ),
        weak: vec![
            "Advance",
            "GoalPause",
            "PauseTakes",
            "Took",
            "LadderMove",
            "GoalResume",
        ],
        strong: vec![],
        mutants: vec!["GoalHoldsTheTab", "LetGoPaused", "ContinuePaused"],
    }
}
