// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quiet upgrade's round as a bounded model: it moves at an idle point with none of the
//! agent's own work running; over background waits it moves at the next point and carries their
//! commands; and real work is told, at a point, the deadline after which the round moves over
//! it, a deadline that stands only while that work holds the round unbroken by a person or a
//! wall.

use aterm_spec::derive::*;

/// One quiet round in a session its autopilot carries, from pending until it moves.
///
/// `posture` is the session's: 0 a turn runs, 1 a settled break of the agent's own background
/// work, 2 an idle point, 3 a wall (a usage limit, the login wall), 4 a person (a box, a draft,
/// their keys, a hold). `work` is what the agent runs under it: 0 nothing the process table or
/// the screen names, 1 only waits, each proved one and named by the command that starts it again
/// (`upgrade::own_of`), 2 real work (anything else; a status its in-process work keeps busy). A
/// break with nothing named is work no row names. `near` is a person's key within their grace,
/// which holds the tell and not the move; `held` that real work has held the round past
/// `upgrade::WORK_TELL_S` with nobody seen, which a look that finds a person or a wall breaks
/// (`HoldBreaks`, strongly fair: a person seen at the breaks the drive looks at, however briefly,
/// is seen); `tells` the one line that names the deadline standing, `due` that deadline passed
/// (`upgrade::WORK_DEADLINE_S`); `carried` that the move took the waits' commands with it. A
/// look that finds a person or a wall, or an idle point with nothing running, voids the deadline
/// and its line and starts the hold again (`DeadlineVoids`, `upgrade::deadline_voids`): work
/// that holds the round again is told again.
///
/// The safety laws read the state a move leaves, whichever action made it: a round moved over
/// real work, or at a break with nothing named, only past a deadline (`NoUnwarnedMoveEndsWork`);
/// one moved over waits carried them (`EndedWaitsAreCarried`); and a deadline passed only where
/// a line named it (`NoDeadlineUntold`). Once moved, nothing changes what the round moved over.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_quiet_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeQuiet {
            const Buggy = 0;
            const MaxTells = 1;
            var posture = 0;
            var work = 0;
            var near = 0;
            var held = 0;
            var tells = 0;
            var due = 0;
            var moved = 0;
            var carried = 0;
            var mute = 0;
            var offpoint = 0;
            var person = 0;
            var early = 0;
            var stuck = 0;
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
            action WaitStarts when (moved == 0 && work == 0) {
                work = 1;
            }
            action RealWorkStarts when (moved == 0 && work <= 1) {
                work = 2;
            }
            action WorkEnds when (moved == 0 && work > 0) {
                work = 0;
            }
            action PersonNear when (moved == 0 && near == 0) {
                near = 1;
            }
            action PersonLeaves when (near == 1) {
                near = 0;
            }
            action HoldOutlasts when (
                moved == 0 && held == 0 && near == 0 &&
                (work == 2 || (posture == 1 && work == 0))
            ) {
                held = 1;
            }
            action HoldBreaks when (moved == 0 && held == 1 && (posture > 2 || near == 1)) {
                held = 0;
            }
            action DeadlinePasses when (moved == 0 && tells > 0 && due == 0) {
                due = 1;
            }

            action DeadlineVoids when (
                moved == 0 && tells > 0 &&
                (posture > 2 || near == 1 || (posture == 2 && work == 0))
            ) {
                tells = 0;
                due = 0;
                held = 0;
            }

            action MoveAtIdle when (moved == 0 && posture == 2 && work == 0) {
                moved = 1;
            }
            action MoveOverOwn when (
                moved == 0 && stuck == 0 && (posture == 1 || posture == 2) &&
                (work == 1 || due == 1)
            ) {
                moved = 1;
                carried = if work == 1 { 1 } else { carried };
            }
            action TellAtPoint when (
                moved == 0 && mute == 0 && held == 1 && near == 0 && tells + 1 <= MaxTells &&
                ((posture == 1 && (work == 0 || work == 2)) || (posture == 2 && work == 2))
            ) {
                tells = tells + 1;
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
                ((posture == 1 && (work == 0 || work == 2)) || (posture == 2 && work == 2))
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
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 2 && work == 2 && due == 0
            ) {
                moved = 1;
                slipped = 1;
            }
            action MoveAtABreak when (
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 1 &&
                (work == 0 || work == 2) && due == 0
            ) {
                moved = 1;
                slipped = 1;
            }
            action WaitsDropped when (
                Buggy == 1 && slipped == 0 && moved == 0 && (posture == 1 || posture == 2) &&
                work == 1
            ) {
                moved = 1;
                slipped = 1;
            }
            action DeadlineUntold when (
                Buggy == 1 && slipped == 0 && moved == 0 && tells == 0 && due == 0
            ) {
                due = 1;
                slipped = 1;
            }
            action TellNeverTyped when (Buggy == 1 && slipped == 0 && mute == 0) {
                mute = 1;
                slipped = 1;
            }
            action OwnWorkHolds when (Buggy == 1 && slipped == 0 && stuck == 0) {
                stuck = 1;
                slipped = 1;
            }

            invariant NoTellOffAPoint: offpoint == 0;
            invariant NoTellOverAPerson: person == 0;
            invariant TellsOnlyPastTheBound: early == 0;
            invariant TellsBounded: tells <= MaxTells;
            invariant NoUnwarnedMoveEndsWork:
                moved == 0 || due == 1 || work == 1 || (work == 0 && posture == 2);
            invariant EndedWaitsAreCarried: moved == 0 || work == 0 || work == 2 || carried == 1;
            invariant NoDeadlineUntold: due == 0 || tells > 0;
            invariant TellsAreTyped: mute == 0;
            invariant OwnWorkYields: stuck == 0;
        }
    }
}

/// The agent's own work never holds a round for good: over waits alone it moves, real work that
/// has held it is told and then moved over at its deadline, and a round not moved is one nothing
/// of the agent's holds, or one its work has not yet held unbroken by a person or a wall (a
/// deadline such a look voided is none). What still holds it past that is a person's or a
/// wall's, which this property does not bound.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_quiet_model)]
pub fn harness_upgrade_quiet_liveness() -> Liveness {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    Liveness {
        name: "OwnWorkNeverHoldsARoundForGood",
        goal: or_(
            or_(is("moved", 1), is("work", 0)),
            and_(and_(is("work", 2), is("due", 0)), is("held", 0)),
        ),
        weak: vec!["BreakSettles", "DeadlinePasses"],
        strong: vec!["TellAtPoint", "HoldBreaks", "MoveOverOwn"],
        mutants: vec!["TellNeverTyped", "OwnWorkHolds"],
    }
}
