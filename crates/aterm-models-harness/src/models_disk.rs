// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk watch's pass: which tier a measured level admits, where each tier
//! stops, and the order of a unit's steps (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §6.1).
//!
//! The Tier-1 bind is `crates/aterm-agent/tests/conformance_disk_pass.rs`: it
//! drives the real `pass_plan` and `apply_auto` with scripted re-measures and
//! removals and holds every step they take to this model's guards.

use aterm_spec::derive::Model;

/// THE PASS. `measured` is the level the look read (0 ok, 1 low, 2 short, 3
/// emergency), once (`looked`); `demand` an agent's adopted wait. `tier` is the
/// tier running (0 none yet, 1 ended owners, 2 idle `incremental/`, 3 whole
/// idle build directories, 4 recent `incremental/`), reached in order through
/// `Admit<n>`; `stopped` says it stopped at its target. `free` is the last
/// re-measure against the running tier's target (0 below the emergency, 1
/// below the floor, 2 below the target, 3 at or above it); tier 1 takes all
/// and reads none. A unit is `Journal` (its intent on record), then for a
/// whole one (tiers 1 and 3) `LockHeld` (every lock of every profile taken and
/// the recheck passed) or `LockRefused`, then `Remove`. The reserve is
/// released before any tier, at a measured emergency only.
///
/// History variables carry what an action did: `unjournalled` (a removal with
/// no intent), `unheld` (a whole removal without its locks), `recent_by` (the
/// measured level plus one when tier 4 was admitted), `released_at` (the level
/// the reserve was released at), `past_target` (a unit removed after free
/// space reached its tier's target), `whole_at_low` (tier 3 admitted at a
/// measured `low` with no demand).
///
/// Each invariant has its own mutant (`Buggy = n`): `RemoveUnjournalled` (1),
/// `RemoveUnheld` (2), `AdmitRecentOnDemand` (3), `ReleaseOnDemand` (4),
/// `JournalPastTarget` (5), `AdmitWholeAtLow` (6).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn disk_reclaim_pass_model() -> Model {
    aterm_spec::ty_model! {
        DiskReclaimPass {
            const Buggy = 0;
            var measured = 0;
            var looked = 0;
            var demand = 0;
            var tier = 0;
            var stopped = 0;
            var intent = 0;
            var held = 0;
            var free = 3;
            var reserve = 1;
            var unjournalled = 0;
            var unheld = 0;
            var recent_by = 0;
            var released_at = 0;
            var past_target = 0;
            var whole_at_low = 0;

            // The look's one measure.
            action MeasureOk when (looked == 0) { looked = 1; measured = 0; free = 3; }
            action MeasureLow when (looked == 0) { looked = 1; measured = 1; free = 2; }
            action MeasureShort when (looked == 0) { looked = 1; measured = 2; free = 1; }
            action MeasureEmergency when (looked == 0) { looked = 1; measured = 3; free = 0; }
            // An agent's wait adopted before the first tier.
            action Adopt when (looked == 1 && tier == 0 && demand == 0) { demand = 1; }
            // The reserve, first, at a measured emergency.
            action ReleaseReserve when (looked == 1 && tier == 0 && reserve == 1 && measured == 3) {
                reserve = 0;
                released_at = measured;
            }
            // The tiers, in order, each at the level that admits it.
            action Admit1 when (looked == 1 && tier == 0 && (measured > 0 || demand == 1)) {
                tier = 1;
                stopped = 0;
            }
            action Admit2 when (
                looked == 1 && tier <= 1 && intent == 0 && (measured > 0 || demand == 1)
            ) {
                tier = 2;
                stopped = 0;
                held = 0;
            }
            action Admit3 when (
                looked == 1 && tier <= 2 && intent == 0 && (measured > 1 || demand == 1)
            ) {
                tier = 3;
                stopped = 0;
                held = 0;
                whole_at_low = if (measured <= 1 && demand == 0) { 1 } else { whole_at_low };
            }
            action Admit4 when (looked == 1 && tier <= 3 && intent == 0 && measured > 1) {
                tier = 4;
                stopped = 0;
                held = 0;
                recent_by = measured + 1;
            }
            // The re-measure before a row of a tier with a target.
            action ReadBelowEmergency when (tier > 1 && stopped == 0 && intent == 0) { free = 0; }
            action ReadBelowFloor when (tier > 1 && stopped == 0 && intent == 0) { free = 1; }
            action ReadBelowTarget when (tier > 1 && stopped == 0 && intent == 0) { free = 2; }
            action ReadAtTarget when (tier > 1 && stopped == 0 && intent == 0) { free = 3; }
            action Stop when (tier > 1 && stopped == 0 && intent == 0 && free == 3) { stopped = 1; }
            // One unit: its intent, its locks (a whole unit), its removal.
            action Journal when (
                tier > 0 && stopped == 0 && intent == 0 && (tier == 1 || free <= 2)
            ) {
                intent = 1;
            }
            action LockHeld when (intent == 1 && held == 0 && (tier == 1 || tier == 3)) {
                held = 1;
            }
            action LockRefused when (intent == 1 && held == 0 && (tier == 1 || tier == 3)) {
                intent = 0;
            }
            action Remove when (intent == 1 && (held == 1 || tier == 2 || tier == 4)) {
                past_target = if (tier > 1 && free == 3) { 1 } else { past_target };
                intent = 0;
                held = 0;
            }

            // Mutants (dead at Buggy = 0), one per invariant.
            action RemoveUnjournalled when (Buggy == 1 && tier > 0 && intent == 0) {
                unjournalled = 1;
            }
            action RemoveUnheld when (
                Buggy == 2 && intent == 1 && held == 0 && (tier == 1 || tier == 3)
            ) {
                unheld = 1;
                intent = 0;
            }
            action AdmitRecentOnDemand when (
                Buggy == 3 && looked == 1 && tier <= 3 && intent == 0 && demand == 1
                    && measured <= 1
            ) {
                tier = 4;
                stopped = 0;
                recent_by = measured + 1;
            }
            action ReleaseOnDemand when (
                Buggy == 4 && looked == 1 && tier == 0 && reserve == 1 && demand == 1
                    && measured <= 2
            ) {
                reserve = 0;
                released_at = measured;
            }
            action JournalPastTarget when (
                Buggy == 5 && tier > 1 && stopped == 0 && intent == 0 && free == 3
            ) {
                intent = 1;
            }
            action AdmitWholeAtLow when (
                Buggy == 6 && looked == 1 && tier <= 2 && intent == 0 && measured == 1
                    && demand == 0
            ) {
                tier = 3;
                stopped = 0;
                whole_at_low = 1;
            }

            invariant NothingWithoutIntent: unjournalled == 0;
            invariant WholeOnlyHeld: unheld == 0;
            invariant RecentOnlyMeasured: recent_by == 0 || recent_by > 2;
            invariant ReserveOnlyMeasuredEmergency: reserve == 1 || released_at == 3;
            invariant StopsAtTarget: past_target == 0;
            invariant NoWholeAtLow: whole_at_low == 0;
            bound TierBox: tier <= 4;
            bound FreeBox: free <= 3;
        }
    }
}

/// THE WAIT (`await sys`, design §3.3). `reply` is 0 while parked, then 1 met,
/// 2 unmet, 3 timed out or 4 stalled; `free_ok` whether the reading the wait
/// last read holds its terms; `beat_old` whether the watch's beat is a minute
/// old; `watch` 0 running, 1 suspended (a seamless successor before its
/// Commit); `pass` the last pass for this wait's demand: 0 none, 1 running,
/// 2 spent (every tier it may, free still short), 3 stopped for another
/// reason; `demand` 1 while the wait's demand is on the board.
///
/// The world moves freely while the wait is parked (readings, beats, the
/// host suspended and back, passes); the wait's answers are `Met`, `Unmet`,
/// `Stall` and `Timeout`, each taking the demand off the board.
///
/// Each invariant has its own mutant (`Buggy = n`): `MetStale` (1, latches on
/// a reading that does not hold), `UnmetStopped` (2, unmet on a pass that
/// stopped for another reason), `StallFresh` (3, stalls while a long pass
/// beats), `StallSuspended` (4, stalls while the host is suspended),
/// `ReplyKeepsDemand` (5, answers and leaves its demand registered),
/// `LoseTimer` (1, a park that drops its deadline: the wait never ends, the
/// liveness's mutant too).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn disk_await_model() -> Model {
    aterm_spec::ty_model! {
        DiskAwait {
            const Buggy = 0;
            var reply = 0;
            var free_ok = 0;
            var beat_old = 0;
            var watch = 0;
            var pass = 0;
            var demand = 1;
            var timer = 1;

            // The world, while the wait is parked.
            action ReadHolds when (reply == 0) { free_ok = 1; }
            action ReadShort when (reply == 0) { free_ok = 0; }
            action BeatFresh when (reply == 0) { beat_old = 0; }
            action BeatOld when (reply == 0) { beat_old = 1; }
            action Suspend when (reply == 0 && watch == 0) { watch = 1; }
            action Resume when (reply == 0 && watch == 1) { watch = 0; beat_old = 0; }
            action PassRuns when (reply == 0 && demand == 1 && pass == 0) { pass = 1; }
            action PassSpent when (reply == 0 && pass == 1) { pass = 2; }
            action PassStops when (reply == 0 && pass == 1) { pass = 3; }
            action PassAgain when (reply == 0 && pass == 3) { pass = 1; }

            // The wait's answers.
            action Met when (reply == 0 && free_ok == 1) { reply = 1; demand = 0; }
            action Unmet when (reply == 0 && free_ok == 0 && demand == 1 && pass == 2) {
                reply = 2;
                demand = 0;
            }
            action Stall when (
                reply == 0 && free_ok == 0 && demand == 1 && beat_old == 1 && watch == 0
            ) {
                reply = 4;
                demand = 0;
            }
            action Timeout when (reply == 0 && timer == 1) { reply = 3; demand = 0; }

            // Mutants (dead at Buggy = 0), one per invariant.
            action MetStale when (Buggy == 1 && reply == 0 && free_ok == 0) {
                reply = 1;
                demand = 0;
            }
            action UnmetStopped when (Buggy == 2 && reply == 0 && free_ok == 0 && pass == 3) {
                reply = 2;
                demand = 0;
            }
            action StallFresh when (
                Buggy == 3 && reply == 0 && demand == 1 && beat_old == 0 && watch == 0
            ) {
                reply = 4;
                demand = 0;
            }
            action StallSuspended when (Buggy == 4 && reply == 0 && demand == 1 && watch == 1) {
                reply = 4;
                demand = 0;
            }
            action ReplyKeepsDemand when (Buggy == 5 && reply == 0 && free_ok == 1) {
                reply = 1;
            }
            action LoseTimer when (Buggy == 1 && reply == 0 && timer == 1) { timer = 0; }

            invariant MetOnlyOnReading: reply <= 0 || reply > 1 || free_ok == 1;
            invariant UnmetOnlySpent: reply <= 1 || reply > 2 || (pass == 2 && free_ok == 0);
            invariant StalledOnlyNoBeat: reply <= 3 || (beat_old == 1 && watch == 0);
            invariant NoDemandAfterReply: reply == 0 || demand == 0;
            invariant TimerArmed: reply > 0 || timer == 1;
            bound ReplyBox: reply <= 4;
            bound PassBox: pass <= 3;
        }
    }
}

/// AN AWAIT ENDS (`[]<>(reply ≠ 0)`): a parked wait answers, at the latest at
/// its timeout. Fairness: `Timeout` (the caller's patience runs out). The
/// world is not fair: readings, beats and passes may go on forever.
/// `LoseTimer` alone breaks it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(disk_await_model)]
pub fn disk_await_liveness() -> aterm_spec::derive::Liveness {
    use aterm_spec::derive::{int, le, var};
    aterm_spec::derive::Liveness {
        name: "AnAwaitEnds",
        goal: le(int(1), var("reply")),
        weak: vec!["Timeout"],
        strong: vec![],
        mutants: vec!["LoseTimer"],
    }
}
