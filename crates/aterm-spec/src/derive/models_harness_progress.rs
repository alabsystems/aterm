// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "No upgrade stuck for ever" as a bounded model
//! (`docs/DESIGN-upgrade-never-stuck-2026-09-28.md` §4): the upgrade moves, or its record names
//! what holds it now. The watch (`upgrade_drive::watch`) puts a timer on every record and
//! `WaitClass` a class on every wait. How the owner is then told (`upgrade_status::Row::attention`)
//! is not modelled; aterm-agent's `the_real_view_is_the_derived_machine` binds it over the states
//! this machine reaches.
//!
//! A sibling of `HarnessUpgradeLadder` (`models_harness.rs`: the move lands, or a floor stands),
//! this one is about the record, not the move: no point steps are assumed to come. Tier-1 binds:
//! aterm-agent's `upgrade_progress_tests.rs` and `upgrade_watch_tests.rs`.

use super::*;

/// The upgrade's progress as its record says it: one conversation's record
/// (`upgrade_drive::St`), its reducer's steps at the points the loop offers, and the host's watch
/// off any point.
///
/// The world (all unfair):
///
/// * `posture`: 0 a turn runs (no point: the loop's `NotIdle`/`NotBusy`); 1 a settled break of
///   the agent's own background work (`host_steps_in_background` → `WorkerIdle::at_background`);
///   2 an idle point (`host_steps_here` → `WorkerIdle::at_idle`); 3 a usage limit's wall
///   (`upgrade::limited`: the loop's `Wall`/`LimitEpisode` withhold its points and every step
///   waits `limited`; a notice standing `LIMIT_GRACE_S` past its known reset is no wall, and the
///   loop offers it as an `IdlePoint`); 4 a person's hold, a box or a draft (the loop's `Prompt`).
/// * `reset`: the wall's named reset (`upgrade::transcript_limit_until`, plus grace) has passed;
///   kept until the next wall.
/// * `work`: processes run under the agent (`Facts::background`, `St::held_by`), the restart's
///   gate (`upgrade::gate_restart`).
/// * `ready`: the agent's READY to the round's notice.
/// * `Window`: the round's clock runs (`REASK_S` since the notice, `RETRY_S` since the stop, a
///   pending round's `due_by`). A clock a limit holds (`upgrade::clock_held`, `St::hold_clock`)
///   is a `Window` that does not come.
///
/// The record:
///
/// * `phase`: 0 Pending, 1 Announced at its last ask (the model's one ask, so a re-ask is the
///   give-up), 2 Failed(`unanswered`) resting to its re-arm, 3 the move (Exiting, Relaunched,
///   Done). `rearmed`: re-armed after a give-up, no notice typed yet (`St::last_stop`).
/// * `cause`: the class of the recorded wait (`WaitClass` of `St::wait`, `Row::overdue_cause`):
///   0 the agent's own work, 1 a person, 2 a limit, 3 aterm's own (the word an act clears,
///   `St::note_step`, and the watch's `upgrade::NEXT_POINT` for a read whose step nothing holds).
/// * `clock`: the round's clock has run.
///
/// The lane:
///
/// * `StepAtIdle`: `upgrade::requested_step` over `upgrade::next_step` at an idle point:
///   announce, give up, re-arm. Unfair: the record must say what holds the move whether or not
///   points come.
/// * `RestartAtIdle`: the same step's restart (`upgrade::gate_restart`).
/// * `StepAtBreak`: `upgrade::break_step` at a settled break: everything but the restart. Unfair.
/// * `Watch`: the host's look off any point (`upgrade_drive::watch`, woken at
///   `upgrade_drive::watch_at` by `upgrade_status::next_change`) and a visit's look at a point
///   (`St::note_step`). It records `T`: for a read that waits, its wait's class; for a read that
///   would act, what holds the point that step waits for (`upgrade::held_point`), else
///   `upgrade::NEXT_POINT` (`upgrade_watch::look_word`). It holds an announced round's clock at
///   a wall whose reset is ahead (`St::hold_clock`) and types nothing. Weakly fair: aterm's own
///   timer.
/// * `WatchRearms`: the watch re-arms a rested round with no hands (`upgrade_drive::rearm`).
///   Unfair.
///
/// The truth `T`, the class of what holds the move now: at a wall, the limit (a resting round's
/// rest there is its own, `failed`); at a person's hold, the person, except the round's own
/// answer inside its window and a resting round's rest; at a turn or a settled break, its own
/// work (the restart is never taken at a break, and the watch's dry read sees the agent at
/// work); at an idle point, aterm's own when the step there would act and has not
/// (`upgrade::NEXT_POINT`), else its own work (`awaiting-ready`, `background`, `failed`).
/// `derived_harness_upgrade_progress_break_is_a_redecision` pins where the break's class differs
/// from counting the ask flow's step there as aterm's own. [`harness_upgrade_progress_truth`] is
/// the same expression, pinned equal to `Watch`'s in the Tier-0 test.
///
/// `RecordedWordStands` (`keep`) is a watch that records a word only when its dry read waits, so
/// a read that would act (`would-announce`, `gave-up`, `would-restart`, `would-rearm`) leaves a
/// cause that no longer holds; it breaks the law alone, and
/// `derived_harness_upgrade_progress_gaps_are_closed_on_main` replays it as a negative control.
/// One neutral word for every act would not do: the give-up and re-arm are taken at any posture,
/// and it would name aterm's own class over a running turn or a person's box. So the dry run says
/// what holds the point after the act (`upgrade::held_point`: `gave-up@box`), and the watch never
/// overwrites a word the act's own checks recorded past where the read stops
/// (`St::held_past_go`, outside this model). This holds for a Claude Code record only: a Codex
/// record's act read records nothing, and the Codex lane is not modelled. The steps are the ask
/// flow's; the quiet flow's words are bound by aterm-agent's
/// `the_quiet_upgrades_read_records_what_holds_its_restart`.
///
/// `NoStaleLook` puts the obligation on the watch: the view is stateless and cannot tell whether
/// the record is current, so the look must record what it read.
///
/// Not modelled: a late READY a gave-up round still hears, the owner's word, the login wall, a
/// taskless restart, a retarget, the Codex lane, the quiet flow (`upgrade_ask = false`, the
/// default), the act's checks past the reducer's go (`St::held_past_go`), a person's hold past
/// the window (`will_ask_again`), a limit record past its named reset (`Row::limit_passed`), the
/// move clock, every tell (`Row::attention`, `next_change`), and the host staying alive:
/// `Watch`'s weak fairness is assumed, and its failure is told at run time by
/// `upgrade_status::Row::unwatched`.
///
/// Safety, each invariant caught by its own dead action alone
/// (`verify::audit_dead_negative_controls`): `NoHandsOffAPoint` (`WatchTypes`: the watch
/// announcing off a point, which the aterm-census guard `watch-hands` also fences),
/// `NoStaleLook` (`RecordedWordStands`), `NeverEndsRunningWork` (`RestartOverWork`). The
/// liveness alone catches `NoLookWithoutAPoint` (looks only at points the session offers). Each
/// mutant fires only from an untouched state (`slipped`), keeping the `Buggy = 1` space inside
/// the interpreter's budget. Liveness: [`harness_upgrade_progress_liveness`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_progress_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeProgress {
            const Buggy = 0;
            var posture = 0;
            var reset = 0;
            var work = 0;
            var ready = 0;
            var phase = 0;
            var rearmed = 0;
            var cause = 0;
            var clock = 0;
            var offpoint = 0;
            var seen = 0;
            var at_work = 0;
            var blind = 0;
            var keep = 0;
            var slipped = 0;

            // The world: what the session shows while the move is owed.
            action TurnRuns when (phase <= 2 && posture > 0) {
                posture = 0;
            }
            action BreakSettles when (phase <= 2 && (posture == 0 || posture > 1)) {
                posture = 1;
            }
            action IdlePoint when (phase <= 2 && (posture <= 1 || posture > 2)) {
                posture = 2;
            }
            action WallStands when (phase <= 2 && (posture <= 2 || posture == 4)) {
                posture = 3;
                reset = 0;
            }
            action PersonHolds when (phase <= 2 && posture <= 3) {
                posture = 4;
            }
            // The wall's reset (and its grace) passes with the notice still on screen: the
            // transcript's named reset, or, none read, the longest a usage limit lasts.
            action ResetPasses when (phase <= 2 && posture == 3 && reset == 0) {
                reset = 1;
            }
            action WorkStarts when (phase <= 2 && work == 0) {
                work = 1;
            }
            action WorkEnds when (phase <= 2 && work == 1) {
                work = 0;
            }
            action Answers when (phase == 1 && ready == 0) {
                ready = 1;
            }
            action Window when (phase <= 2 && clock == 0) {
                clock = 1;
            }
            // The step at an idle point (`requested_step`): announce, give up (window run, no
            // restart to take), or re-arm (rested). An act clears the recorded word; `offpoint`
            // latches a phase moved off a point.
            action StepAtIdle when (
                posture == 2 && (
                    phase == 0 ||
                    (phase == 1 && clock == 1 && (ready == 0 || work == 1)) ||
                    (phase == 2 && clock == 1)
                )
            ) {
                phase = if phase == 0 { 1 } else if phase == 1 { 2 } else { 0 };
                rearmed = if phase == 2 { 1 } else { 0 };
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
            }
            // The same step's restart (`upgrade::gate_restart`): READY heard, nothing under the
            // agent. `at_work` latches work under it, `offpoint` a move off a point.
            action RestartAtIdle when (
                posture == 2 && phase == 1 && ready == 1 && work == 0
            ) {
                phase = 3;
                rearmed = 0;
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                at_work = work;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
            }
            // The step at a settled break (`break_step`): never the restart.
            action StepAtBreak when (
                posture == 1 && (phase == 0 || ((phase == 1 || phase == 2) && clock == 1))
            ) {
                phase = if phase == 0 { 1 } else if phase == 1 { 2 } else { 0 };
                rearmed = if phase == 2 { 1 } else { 0 };
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
            }

            // The watch's look off any point, and a visit's at one: it records `T`. Under `keep`
            // it records only what its dry read waits on, so a read that would act leaves the
            // word; under `blind` nothing wakes it off a point.
            action Watch when (phase <= 2) {
                cause = if blind == 1 && (posture == 0 || posture > 2) { cause }
                    else if keep == 1 && (
                            if phase == 0 { posture == 2 }
                            else if phase == 1 {
                                (posture == 2 && ready == 1 && work == 0) ||
                                (clock == 1 && (posture <= 2 || posture == 4) &&
                                    (ready == 0 || work == 1))
                            }
                            else { clock == 1 }
                        ) { cause }
                    else { (
                            if posture == 3 { if phase == 2 { 0 } else { 2 } }
                            else if posture == 4 {
                                if phase == 0 { 1 } else if phase == 2 { 0 }
                                else if clock == 1 || (ready == 1 && work == 0) { 1 } else { 0 }
                            }
                            else if posture <= 1 { 0 }
                            else if phase == 0 || clock == 1 ||
                                (phase == 1 && ready == 1 && work == 0) { 3 }
                            else { 0 }
                        ) };
                // The limit hold (`St::hold_clock`; a visit's `upgrade::clock_held`): at a wall
                // whose reset is ahead, an announced round's clock restarts at the reset.
                clock = if posture == 3 && phase == 1 && reset == 0 && blind == 0 { 0 } else { clock };
                // `T` as the look saw it, whatever it recorded: `NoStaleLook` holds `cause` to it.
                seen = if posture == 3 { if phase == 2 { 0 } else { 2 } }
                    else if posture == 4 {
                        if phase == 0 { 1 } else if phase == 2 { 0 }
                        else if clock == 1 || (ready == 1 && work == 0) { 1 } else { 0 }
                    }
                    else if posture <= 1 { 0 }
                    else if phase == 0 || clock == 1 ||
                        (phase == 1 && ready == 1 && work == 0) { 3 }
                    else { 0 };
            }
            // The watch's re-arm (`upgrade_drive::rearm`): a rested round, with no hands. The
            // same pass then records the new round's word (`Watch`).
            action WatchRearms when (
                phase == 2 && clock == 1 && (blind == 0 || posture == 1 || posture == 2)
            ) {
                phase = 0;
                rearmed = 1;
                clock = 0;
            }

            // Mutants, each its own dead action, each firing once (`slipped`). No look but at
            // a point the session offers (`next_change` without its `watch_at` arm).
            action NoLookWithoutAPoint when (
                Buggy == 1 && slipped == 0 && blind == 0 &&
                (posture == 0 || posture == 3 || posture == 4)
            ) {
                blind = 1;
                slipped = 1;
            }
            // The watch records only a read that waits.
            action RecordedWordStands when (Buggy == 1 && slipped == 0 && keep == 0) {
                keep = 1;
                slipped = 1;
            }
            // The watch with hands: a notice typed off a point.
            action WatchTypes when (
                Buggy == 1 && slipped == 0 && phase == 0 &&
                (posture == 0 || posture == 3 || posture == 4)
            ) {
                phase = 1;
                clock = 0;
                cause = 3;
                seen = 3;
                offpoint = if posture == 0 || posture > 2 { 1 } else { offpoint };
                slipped = 1;
            }
            // The restart over processes under the agent.
            action RestartOverWork when (
                Buggy == 1 && slipped == 0 && posture == 2 && phase == 1 &&
                ready == 1 && work == 1
            ) {
                phase = 3;
                ready = 0;
                clock = 0;
                cause = 3;
                seen = 3;
                at_work = work;
                slipped = 1;
            }

            invariant NoHandsOffAPoint: offpoint == 0;
            invariant NoStaleLook: cause == seen;
            invariant NeverEndsRunningWork: at_work == 0;
        }
    }
}

/// `T` of [`harness_upgrade_progress_model`], the class of what holds the move now: the same
/// expression the healthy `Watch` records, pinned equal to it over every reachable state by
/// `derived_harness_upgrade_progress.rs`, so the yardstick and the look cannot drift apart.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_progress_truth() -> Expr {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    if_(
        is("posture", 3),
        if_(is("phase", 2), int(0), int(2)),
        if_(
            is("posture", 4),
            if_(
                is("phase", 0),
                int(1),
                if_(
                    is("phase", 2),
                    int(0),
                    if_(
                        or_(is("clock", 1), and_(is("ready", 1), is("work", 0))),
                        int(1),
                        int(0),
                    ),
                ),
            ),
            if_(
                le(var("posture"), int(1)),
                int(0),
                if_(
                    or_(
                        or_(is("phase", 0), is("clock", 1)),
                        and_(is("phase", 1), and_(is("ready", 1), is("work", 0))),
                    ),
                    int(3),
                    int(0),
                ),
            ),
        ),
    )
}

/// The upgrade moves, or its record names what holds it: `[]<>(moved \/ cause = T)`. A behaviour
/// that leaves it for good keeps a cause that no longer holds, look after look.
///
/// The world and the points are not fair: the steps at a point may never come, and the record
/// must stay true whether or not they do. The verdict assumes only `Watch`, weakly fair: the
/// host's timer wakes at every record's `watch_at` (`next_change`'s `watch_at` arm,
/// `View::watch_due`) on the wall clock, so an instant slept through is past at the wake
/// (aterm-gui's `bell_wait`); where it never fires the view says so (`unwatched`).
///
/// Each mutant breaks it alone: `NoLookWithoutAPoint` (a wall after a posture change is never
/// looked at again) and `RecordedWordStands` (a stale word where the read would act). Both are
/// also a bad state once they bite (`NoStaleLook`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::harness_upgrade_progress_model)]
pub fn harness_upgrade_progress_liveness() -> Liveness {
    let is = |v: &'static str, n: i64| eq(var(v), int(n));
    Liveness {
        name: "TheUpgradeMovesOrTheRecordNamesWhatHoldsIt",
        goal: or_(
            is("phase", 3),
            eq(var("cause"), harness_upgrade_progress_truth()),
        ),
        weak: vec!["Watch"],
        strong: vec![],
        mutants: vec!["NoLookWithoutAPoint", "RecordedWordStands"],
    }
}
