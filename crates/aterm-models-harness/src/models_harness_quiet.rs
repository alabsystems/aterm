// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quiet upgrade's round as a bounded model (`docs/DESIGN-upgrade-never-stuck-2026-09-28.md`,
//! "Quiet tell"): it moves only at an idle point with none of the agent's own work running, and
//! once that work has held it past the bound, the agent is told what waits, at a point, a bounded
//! number of times.

use aterm_spec::derive::*;

/// One quiet round, from pending until it moves.
///
/// `posture` is the session's: 0 a turn runs, 1 a settled break of the agent's own background
/// work, 2 an idle point, 3 a wall (a usage limit, the login wall), 4 a person (a box, a draft,
/// their keys, a hold). `work` is the agent's own work running under it at an idle point (a
/// process, or a status its in-process work keeps busy); `near` a person's key within their grace,
/// which holds the tell and not the land rung's move; `held` that work has held the round past
/// `upgrade::OWN_TELL_S` at the land rung with nobody seen, which a look that finds a person or a
/// wall breaks (`HoldBreaks`, strongly fair: a person seen at the breaks the drive looks at, however
/// briefly, is seen); `rest` the time after a tell before the next may go (`upgrade::RETELL_S`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_quiet_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeQuiet {
            const Buggy = 0;
            const MaxTells = 3;
            var posture = 0;
            var work = 0;
            var near = 0;
            var held = 0;
            var tells = 0;
            var rest = 0;
            var moved = 0;
            var mute = 0;
            var offpoint = 0;
            var person = 0;
            var early = 0;
            var at_work = 0;
            var slipped = 0;

            action TurnRuns when (moved == 0 && posture > 0) {
                posture = 0;
            }
            action BreakSettles when (moved == 0 && (posture == 0 || posture > 1)) {
                posture = 1;
            }
            action IdlePoint when (moved == 0 && (posture <= 1 || posture > 2)) {
                posture = 2;
            }
            action WallStands when (moved == 0 && (posture <= 2 || posture == 4)) {
                posture = 3;
            }
            action PersonHolds when (moved == 0 && posture <= 3) {
                posture = 4;
            }
            action WorkStarts when (moved == 0 && work == 0) {
                work = 1;
            }
            action WorkEnds when (moved == 0 && work == 1) {
                work = 0;
            }
            action PersonNear when (moved == 0 && near == 0) {
                near = 1;
            }
            action PersonLeaves when (near == 1) {
                near = 0;
            }
            action HoldOutlasts when (
                moved == 0 && held == 0 && near == 0 && (posture <= 1 || (posture == 2 && work == 1))
            ) {
                held = 1;
            }
            action HoldBreaks when (moved == 0 && held == 1 && (posture > 2 || near == 1)) {
                held = 0;
            }
            action RestRuns when (rest == 1) {
                rest = 0;
            }

            action MoveAtIdle when (moved == 0 && posture == 2 && work == 0) {
                moved = 1;
                at_work = work;
            }
            action TellAtPoint when (
                moved == 0 && mute == 0 && held == 1 && near == 0 && tells + 1 <= MaxTells &&
                rest == 0 && (posture == 1 || (posture == 2 && work == 1))
            ) {
                tells = tells + 1;
                rest = 1;
            }

            action TellOffAPoint when (
                Buggy == 1 && slipped == 0 && moved == 0 && held == 1 &&
                (posture == 0 || posture == 3)
            ) {
                offpoint = 1;
                slipped = 1;
            }
            action TellOverAPerson when (
                Buggy == 1 && slipped == 0 && moved == 0 && held == 1 && (posture == 4 || near == 1)
            ) {
                person = 1;
                slipped = 1;
            }
            action TellsEarly when (
                Buggy == 1 && slipped == 0 && moved == 0 && held == 0 &&
                (posture == 1 || (posture == 2 && work == 1))
            ) {
                early = 1;
                slipped = 1;
            }
            action TellsPastTheCap when (
                Buggy == 1 && slipped == 0 && moved == 0 && MaxTells <= tells
            ) {
                tells = tells + 1;
                slipped = 1;
            }
            action MoveOverWork when (
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 2 && work == 1
            ) {
                moved = 1;
                at_work = 1;
                slipped = 1;
            }
            action MoveAtABreak when (
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 1
            ) {
                moved = 1;
                at_work = 1;
                slipped = 1;
            }
            action TellNeverTyped when (Buggy == 1 && slipped == 0 && mute == 0) {
                mute = 1;
                slipped = 1;
            }

            invariant NoTellOffAPoint: offpoint == 0;
            invariant NoTellOverAPerson: person == 0;
            invariant TellsOnlyPastTheBound: early == 0;
            invariant TellsBounded: tells <= MaxTells;
            invariant NeverEndsRunningWork: at_work == 0;
            invariant TellsAreTyped: mute == 0;
        }
    }
}

/// A round the agent's own work holds past the bound is told, or moves, or a person or a wall
/// takes it back: what aterm can promise. The move itself waits on the agent's work ending.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_quiet_model)]
pub fn harness_upgrade_quiet_liveness() -> Liveness {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    Liveness {
        name: "AHeldQuietRoundIsToldOrMoves",
        goal: or_(or_(is("moved", 1), le(int(1), var("tells"))), is("held", 0)),
        weak: vec!["BreakSettles"],
        strong: vec!["TellAtPoint", "HoldBreaks"],
        mutants: vec!["TellNeverTyped"],
    }
}
