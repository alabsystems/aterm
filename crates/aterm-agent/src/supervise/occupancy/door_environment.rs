// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `OccupancyDoor` (`aterm_models_harness::occupancy_door_model`) that no function
//! here performs. The loop's own bind where the real code runs: `Intend` and `Lapse` on
//! [`super::decide`], `IdleRead` and `HoldCloses` on [`super::HoldClock::step`], `Crash` on
//! [`crate::supervise::facts::FactsState::seed`], `Type` on the loop's fenced write
//! (`Session::type_fact`) and `Skip` on its skipped outcome (`Session::door_skipped`), each
//! projected by the Tier-1 bind (`tests/conformance_occupancy.rs`). Proof and test builds only.

/// The world, the loop's steps not built yet and the mutants of `OccupancyDoor`: the agent's, the
/// person's, the machine's, the host's and the clock's steps the Tier-1 bind mirrors, the K Entry
/// that nothing implements yet, and each `Buggy` member.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Turn",
        reason = "A live turn at the door: the agent's or a person's step. The Tier-1 bind feeds the hold's clock the turn answering a harness line as the harness's own (HoldEvent::LiveTurn), and the decision of the point it leaves."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Break",
        reason = "A background break beginning: the agent's step. The Tier-1 bind feeds the hold's clock the break's first settled read exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Point",
        reason = "An idle point: the agent's step. The loop decides only at a drawn break, so the Tier-1 bind takes no line there; the point's own decision is fed to the hold's clock once, at HoldCloses or as the door turns."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "HoldAges",
        reason = "The open hold passing the rule's age: the clock. The Tier-1 bind moves its clock past the age exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "PersonIn",
        reason = "A person's grace, a driver's hand, hold=1 or a draft: a person's step. The Tier-1 bind sets that guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "PersonOut",
        reason = "That hand lifted: a person's step. The Tier-1 bind clears the guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TaskEnds",
        reason = "A session with no task, or a person's own exchange: a person's step. The Tier-1 bind sets that guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TaskBegins",
        reason = "A task given: a person's step. The Tier-1 bind clears the guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "BrakeOn",
        reason = "A brake of the break's own (a stall, the memory wall, reach Down, credits): the machine's and the loop's state, judged before the decider. The Tier-1 bind sets that guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "BrakeOff",
        reason = "That brake lifted. The Tier-1 bind clears the guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "ActOwed",
        reason = "An act standing untaken: the agent's step. The Tier-1 bind sets Guard::ActUntaken exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "ActTaken",
        reason = "That act taken: the agent's step. The Tier-1 bind clears the guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Holds",
        reason = "A rule's condition met on the board: the machine's. The Tier-1 bind sets the reading's room exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Fails",
        reason = "A rule's condition failing: the board's room gone, or the agent's WAITING: line. The Tier-1 bind sets the reading or declares the hold exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "UpgradeDue",
        reason = "The upgrade's notice coming due: the host's state, which IdleHost::upgrade_due answers after the grant. The Tier-1 bind sets the host's answer exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "PaceLapses",
        reason = "The session's pace running out: the clock. The Tier-1 bind moves its clock past the pace exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "OpenNudge",
        reason = "A rule's condition met in an episode not yet told: what decide reads from the hold and the books in the same call that makes Intend's verdict; the Tier-1 bind opens it at that read."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "OpenWake",
        reason = "A declared wake met (§4.1), not implemented yet. The Tier-1 bind walks the built-in rules only; a wake is Tier-0 only."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Close",
        reason = "An episode past its dwell: a new hold for the books (Books::hold), or hold-room's spacing in decide. The Tier-1 bind moves the hold or the clock exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "UpgradeTypes",
        reason = "The upgrade's notice typed at the break: the host's own step (IdleHost::at_background), which the break's step takes before the door's write and which holds the door's line there (Guard::HostLine). The Tier-1 bind sets the host's line and the clock's answer owed exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "Sample",
        reason = "One look toward the K Entry, not implemented yet."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "SampleLast",
        reason = "The K-th look, raising the Entry, not implemented yet."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeAtAnyDoor",
        reason = "Buggy=2 negative control only; a line typed into a live turn"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeUnderPerson",
        reason = "Buggy=3 negative control only; a line typed over a person's hand, which decide holds (Guard::Person)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeWithoutAJob",
        reason = "Buggy=4 negative control only; a line typed into a taskless session, which decide holds (Guard::Taskless)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeUnderOff",
        reason = "Buggy=5 negative control only; a nudge typed outside Cruise, which decide holds (Guard::Autopilot)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeThroughBrake",
        reason = "Buggy=6 negative control only; a line typed through a brake, which decide holds"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeOverUntaken",
        reason = "Buggy=7 negative control only; a line typed over an untaken act, which decide holds (Guard::ActUntaken)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeUnpaced",
        reason = "Buggy=8 negative control only; a line typed inside the session's pace, which decide paces"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeWithoutRow",
        reason = "Buggy=9 negative control only; a line typed with no intent row before it"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeStale",
        reason = "Buggy=10 negative control only; a line typed after its condition failed, which decide drops"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "StackSecond",
        reason = "Buggy=11 negative control only; a second line at one door"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeKeepsDue",
        reason = "Buggy=12 negative control only; an episode left due after its line, which the books close"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "SeedIntentAsTold",
        reason = "Buggy=13 negative control only; an intent row seeded as told, which FactsState::seed leaves untold (the Tier-1 bind plants it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "UpgradeFirst",
        reason = "Buggy=14 negative control only; the upgrade's notice typed over a due wake"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "LateRaise",
        reason = "Buggy=15 negative control only; the K-th look that raises nothing"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "RestartOnIdle",
        reason = "Buggy=16 negative control only; a hold restarted at a new break, which HoldClock::step never does (the Tier-1 bind plants it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "CloseOnOurTurn",
        reason = "Buggy=17 negative control only; a hold ended at the turn answering a harness line, which HoldClock::step never does (the Tier-1 bind plants it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "CloseAtTheAnswer",
        reason = "Buggy=18 negative control only; a hold ended at the point that ends a harness line's answer, which HoldClock::step never does (the Tier-1 bind plants it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "CloseEarly",
        reason = "Buggy=19 negative control only; an episode closed while its condition holds, which Books::hold never does within a hold"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "TypeOverNotice",
        reason = "Buggy=20 negative control only; a nudge typed while the upgrade's notice is due"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "NudgeUnheld",
        reason = "Buggy=21 negative control only; a nudge opened before its hold's age, which decide keeps quiet"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "OpenStuck",
        reason = "A fresh facts reading (IdleHost::facts) naming a hung call whose episode is untold, under the owner's stuck_break_note: what FactsState::pending reads in the same break that asks stuck_at_break. The Tier-1 bind's stuck runs open it at that read."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "StuckWithoutKey",
        reason = "Buggy=22 negative control only; a stuck episode opened with the key off, which stuck_at_break keeps Ungranted"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "RestartClock",
        reason = "Buggy=1 liveness mutant only; a due line's looks restarted"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OccupancyDoor",
        action = "LoseDeadline",
        reason = "Buggy=1 liveness mutant only; a due line with no deadline"
    )
)]
#[doc(hidden)]
pub fn occupancy_door_environment() {}
