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
/// (`upgrade::own_of`), 2 real work (any other process), 3 what names nothing that would be lost:
/// only follows and waits, at least one a follow (`upgrade::follows_only`: a `tail -f`, a
/// development server), or a status its in-process work keeps busy with no process under it
/// (`upgrade::status_lags`). A working session tells 3 as it tells real work. A break with nothing
/// named is work no row names. `near` is a person's key within their grace,
/// which holds the tell and not the move; `held` that real work has held the round past
/// `upgrade::WORK_TELL_S` with nobody seen, which a look that finds a person or a wall breaks
/// (`HoldBreaks`, strongly fair: a person seen at the breaks the drive looks at, however briefly,
/// is seen); `tells` the one line that names the deadline standing, `due` that deadline passed
/// (`upgrade::WORK_DEADLINE_S`); `carried` that the move took the waits' commands with it. A
/// look that finds a person or a wall, or an idle point with nothing running, voids the deadline
/// and its line and starts the hold again (`DeadlineVoids`, `upgrade::deadline_voids`): work
/// that holds the round again is told again.
///
/// `off` is a person's hold on the carry (`upgrade::Facts::may_tell` clear: the tab's Autopilot
/// off, a person's grace or another driver's hand, under Land the person's own exchange), which
/// holds the tell and the move over the agent's own for as long as it stands (neither action is
/// fair: a person may keep it for good). `managed` is a manager waiting on the session, which
/// stands for its loop's life and holds neither: the autopilot carries a managed worker as it
/// carries any other (`ManagersNeverHoldTheCarry`).
///
/// `dated` is a wall its transcript dates (a login wall, `upgrade::LoginScan::since`; a usage
/// limit, `upgrade::StandingScan::since`), and `aged` that it has stood its bound
/// (`upgrade::LOGIN_WALL_S`, `upgrade::LIMIT_WALL_S`) with nobody seen: no person's and no work
/// of the agent's, so the round moves there as at an idle point with nothing of its own running
/// (`WallsYield`); a person near restarts its clock. A wall only the screen shows (`dated` clear)
/// is not bounded here.
///
/// `silent` is a session whose task finished or landed, into which nothing is typed before a move
/// (`upgrade::Facts::silent`): no deadline is told there, and `outlasted`, its own follows' hold
/// unbroken past `upgrade::SILENT_HOLD_S` (the earliest a told deadline would let a working
/// session move), moves the round over them instead, unless a deadline told before stands
/// (`SilentWorkYields`, `NoTellIntoASilentSession`). Its real work holds the round: nothing warns
/// it there, and ending it would lose what it has not finished (`NoUnwarnedMoveEndsWork`).
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
            var off = 0;
            var managed = 0;
            var mheld = 0;
            var silent = 0;
            var dated = 0;
            var aged = 0;
            var wstuck = 0;
            var outlasted = 0;
            var hush = 0;
            var loud = 0;

            action TurnRuns when (moved == 0 && posture > 0) {
                posture = 0;
                dated = 0;
                aged = 0;
            }
            action BreakSettles when (moved == 0 && (posture == 0 || posture > 1)) {
                posture = 1;
                dated = 0;
                aged = 0;
            }
            action IdlePoint when (moved == 0 && (posture <= 1 || posture > 2)) {
                posture = 2;
                dated = 0;
                aged = 0;
            }
            action WallStands when (moved == 0 && (posture <= 2 || posture == 4)) {
                posture = 3;
                dated = 0;
                aged = 0;
            }
            action DatedWallStands when (moved == 0 && (posture <= 2 || posture == 4)) {
                posture = 3;
                dated = 1;
                aged = 0;
            }
            action WallAges when (
                moved == 0 && posture == 3 && dated == 1 && aged == 0 && near == 0
            ) {
                aged = 1;
            }
            action PersonHolds when (moved == 0 && posture <= 3) {
                posture = 4;
                dated = 0;
                aged = 0;
            }
            action WaitStarts when (moved == 0 && work == 0) {
                work = 1;
            }
            action RealWorkStarts when (moved == 0 && work <= 1) {
                work = 2;
            }
            action FollowStarts when (moved == 0 && work <= 1) {
                work = 3;
            }
            action WorkEnds when (moved == 0 && work > 0) {
                work = 0;
            }
            action PersonNear when (moved == 0 && near == 0) {
                near = 1;
                aged = 0;
            }
            action PersonLeaves when (near == 1) {
                near = 0;
            }
            action HoldOutlasts when (
                moved == 0 && held == 0 && near == 0 &&
                (work > 1 || (posture == 1 && work == 0))
            ) {
                held = 1;
            }
            action HoldBreaks when (moved == 0 && held == 1 && (posture > 2 || near == 1)) {
                held = 0;
                outlasted = 0;
            }
            action SilentHoldOutlasts when (
                moved == 0 && held == 1 && outlasted == 0 && near == 0 && posture <= 2
            ) {
                outlasted = 1;
            }
            action TaskLands when (moved == 0 && silent == 0) {
                silent = 1;
            }
            action TaskAsked when (moved == 0 && silent == 1) {
                silent = 0;
            }
            action DeadlinePasses when (moved == 0 && tells > 0 && due == 0) {
                due = 1;
            }
            action CarryHeld when (moved == 0 && off == 0) {
                off = 1;
            }
            action CarryBack when (off == 1 && mheld == 0) {
                off = 0;
            }
            action ManagerWaits when (moved == 0 && managed == 0) {
                managed = 1;
            }

            action DeadlineVoids when (
                moved == 0 && tells > 0 &&
                (posture > 2 || near == 1 || (posture == 2 && work == 0))
            ) {
                tells = 0;
                due = 0;
                held = 0;
                outlasted = 0;
            }

            action MoveAtIdle when (
                moved == 0 && work == 0 &&
                (posture == 2 || (posture == 3 && dated == 1 && aged == 1 && wstuck == 0))
            ) {
                moved = 1;
            }
            action MoveOverOwn when (
                moved == 0 && stuck == 0 && off == 0 && (posture == 1 || posture == 2) &&
                (work == 1 || due == 1 ||
                    (silent == 1 && hush == 0 && outlasted == 1 && tells == 0 && near == 0 &&
                        (work == 3 || (posture == 1 && work == 0))))
            ) {
                moved = 1;
                carried = if work == 1 { 1 } else { carried };
            }
            action TellAtPoint when (
                moved == 0 && mute == 0 && off == 0 && silent == 0 && held == 1 && near == 0 &&
                tells + 1 <= MaxTells &&
                ((posture == 1 && (work == 0 || work > 1)) || (posture == 2 && work > 1))
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
                ((posture == 1 && (work == 0 || work > 1)) || (posture == 2 && work > 1))
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
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 2 && due == 0 &&
                (work == 2 || (work == 3 && (silent == 0 || outlasted == 0 || tells > 0)))
            ) {
                moved = 1;
                slipped = 1;
            }
            action MoveAtABreak when (
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 1 && due == 0 &&
                (work == 2 ||
                    ((work == 0 || work == 3) && (silent == 0 || outlasted == 0 || tells > 0)))
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
            action SilentWorkHolds when (Buggy == 1 && slipped == 0 && hush == 0) {
                hush = 1;
                slipped = 1;
            }
            action SilentMovesEarly when (
                Buggy == 1 && slipped == 0 && moved == 0 && silent == 1 && outlasted == 0 &&
                due == 0 && (posture == 1 || posture == 2) && work == 3
            ) {
                moved = 1;
                slipped = 1;
            }
            action TellsASilentSession when (
                Buggy == 1 && slipped == 0 && moved == 0 && silent == 1 && held == 1 && near == 0 &&
                ((posture == 1 && (work == 0 || work > 1)) || (posture == 2 && work > 1))
            ) {
                loud = 1;
                slipped = 1;
            }
            action MovesAtALiveWall when (
                Buggy == 1 && slipped == 0 && moved == 0 && posture == 3 && dated == 1 &&
                aged == 0 && work == 0
            ) {
                moved = 1;
                slipped = 1;
            }
            action WallHoldsForGood when (Buggy == 1 && slipped == 0 && wstuck == 0) {
                wstuck = 1;
                slipped = 1;
            }
            action ManagerHoldsCarry when (
                Buggy == 1 && slipped == 0 && moved == 0 && managed == 1 && off == 0
            ) {
                off = 1;
                mheld = 1;
                slipped = 1;
            }

            invariant NoTellOffAPoint: offpoint == 0;
            invariant NoTellOverAPerson: person == 0;
            invariant TellsOnlyPastTheBound: early == 0;
            invariant TellsBounded: tells <= MaxTells;
            invariant NoUnwarnedMoveEndsWork:
                moved == 0 || due == 1 || work == 1 ||
                (work == 0 && (posture == 2 || (posture == 3 && dated == 1 && aged == 1))) ||
                (silent == 1 && outlasted == 1 && tells == 0 && (work <= 1 || work == 3));
            invariant EndedWaitsAreCarried: moved == 0 || work == 0 || work > 1 || carried == 1;
            invariant NoDeadlineUntold: due == 0 || tells > 0;
            invariant TellsAreTyped: mute == 0;
            invariant OwnWorkYields: stuck == 0;
            invariant ManagersNeverHoldTheCarry: mheld == 0;
            invariant SilentWorkYields: hush == 0;
            invariant NoTellIntoASilentSession: loud == 0;
            invariant WallsYield: wstuck == 0;
        }
    }
}

/// The agent's own work never holds a round for good: over waits alone it moves, real work and
/// follows that have held it are told and then moved over at their deadline (in a session nothing
/// is typed into, follows are moved over once their hold outlasts that deadline's bound), and a
/// round not moved is one nothing of the agent's holds, or one its work has not yet held unbroken
/// by a person or a wall (a deadline such a look voided is none). What still holds it past that
/// is a person's, a wall's or work that would be lost, which this property does not bound: a
/// person's hold on the carry (`off`) and real work in a session nothing warns (`silent`) are in
/// the goal, a manager's wait is not.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_quiet_model)]
pub fn harness_upgrade_quiet_liveness() -> Liveness {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    Liveness {
        name: "OwnWorkNeverHoldsARoundForGood",
        goal: or_(
            or_(
                or_(is("moved", 1), is("work", 0)),
                and_(
                    and_(or_(is("work", 2), is("work", 3)), is("due", 0)),
                    is("held", 0),
                ),
            ),
            or_(
                and_(is("off", 1), is("mheld", 0)),
                and_(is("silent", 1), is("work", 2)),
            ),
        ),
        weak: vec!["BreakSettles", "DeadlinePasses", "SilentHoldOutlasts"],
        strong: vec!["TellAtPoint", "HoldBreaks", "MoveOverOwn"],
        mutants: vec![
            "TellNeverTyped",
            "OwnWorkHolds",
            "ManagerHoldsCarry",
            "SilentWorkHolds",
        ],
    }
}

/// A wall nobody lifts that its transcript dates (a login wall, a usage limit) never holds an
/// idle round for good: with nothing of the agent's own running and nobody near, a wall that
/// stands ages past its bound (`upgrade::LOGIN_WALL_S`, `upgrade::LIMIT_WALL_S`) and the round
/// moves. A wall only the screen shows, a person near and the agent's own work are outside it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_quiet_model)]
pub fn harness_upgrade_wall_liveness() -> Liveness {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    Liveness {
        name: "AWallNeverHoldsAnIdleRoundForGood",
        goal: or_(
            or_(is("moved", 1), neq(var("posture"), int(3))),
            or_(or_(is("dated", 0), neq(var("work"), int(0))), is("near", 1)),
        ),
        weak: vec!["WallAges", "MoveAtIdle"],
        strong: vec![],
        mutants: vec!["WallHoldsForGood"],
    }
}
