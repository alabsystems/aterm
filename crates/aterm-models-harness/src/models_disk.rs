// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk watch's pass.

use aterm_spec::derive::Model;

/// THE PASS.
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

            action MeasureOk when (looked == 0) { looked = 1; measured = 0; free = 3; }
            action MeasureLow when (looked == 0) { looked = 1; measured = 1; free = 2; }
            action MeasureShort when (looked == 0) { looked = 1; measured = 2; free = 1; }
            action MeasureEmergency when (looked == 0) { looked = 1; measured = 3; free = 0; }
            action Adopt when (looked == 1 && tier == 0 && demand == 0) { demand = 1; }
            action ReleaseReserve when (looked == 1 && tier == 0 && reserve == 1 && measured == 3) {
                reserve = 0;
                released_at = measured;
            }
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
            action ReadBelowEmergency when (tier > 1 && stopped == 0 && intent == 0) { free = 0; }
            action ReadBelowFloor when (tier > 1 && stopped == 0 && intent == 0) { free = 1; }
            action ReadBelowTarget when (tier > 1 && stopped == 0 && intent == 0) { free = 2; }
            action ReadAtTarget when (tier > 1 && stopped == 0 && intent == 0) { free = 3; }
            action Stop when (tier > 1 && stopped == 0 && intent == 0 && free == 3) { stopped = 1; }
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

/// THE WAIT (`await sys`, design §3.3).
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

/// AN AWAIT ENDS (`[]<>(reply ≠ 0)`): a parked wait answers, at the latest at its timeout.
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
