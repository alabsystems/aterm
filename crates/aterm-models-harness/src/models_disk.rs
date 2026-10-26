// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk watch's pass, its waits, and its ask to free space.

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

/// THE ASK (`disk-free@v1`, `docs/DESIGN-disk-free-ask-2026-10-24.md`): one machine-wide lease
/// over three sessions A, B and C, whichever hosts they run on, read by two hosts whose floors on
/// the volume differ: H's lies past L's by more than the margin an episode ends past. Neither
/// host ends, so the floor an episode is held to is only ever raised.
/// - `free`: below both floors (0), within the margin above L's (1), past that and below H's (2),
///   within the margin above H's (3), past it (4);
/// - `bar`: the floor the lease holds the episode to, L's (1) or H's (2): the opener's, raised
///   by any later look of H's that does not end it;
/// - `done`: aterm's own freeing is done there; `dA`: A's task is done or exited, so the seat
///   rule bars it;
/// - `phase`: no episode (0), nominating (1), owed to `owner` (2), asked (3), settled (4),
///   out (5); `aged`: a settled or out episode parked a cooldown ago, so a look that reads the
///   volume short opens a new one in its place (`Expire`, then the opening);
/// - `pX`: X was elected this episode; `lX`: the lines X took this episode; `asks`: the lines
///   the lease counted; `cool`: a line went within the cooldown; `rose`: the asked session freed
///   a GiB since a line (what aterm's own passes free meanwhile, on either host, recorded in the
///   lease or still under way, the lease takes out of the rise it judges, so to the model it is
///   no rise and no step: `rose` stays 0); `raised`: the owner's disk line says the asks ran out.
///   A's `pA` says more: 1,
///   elected; 2, gone back to the episode as its host held it while owed (`ReturnA`), so it may
///   be elected once more; 3, elected that once more and owed. A 3 is kept only while A is owed,
///   and a 2 is dropped as the episode parks, as nothing there reads them;
/// - `uA`: A's line went but its host could not write its record (a full disk), so the lease
///   still owes it, or has handed it off, run out, or elected another, until that host's next
///   look records it: the record counts it as one of the episode's lines, and the episode waits
///   on A's answer again (`RecordA`, `RecordBackA`) unless another session elected since may
///   still take the episode's second line (`RecordOverA`); one elected since whose line would be
///   past the two is withdrawn untyped (`RecordWithdrawA`). The record lands before another
///   session's line goes (`TypeB` and `TypeC` wait on it): a host looks a minute apart
///   (`LOOK_EVERY_MS`), and design §7 names a record lost for longer;
/// - `wA`: the lease's record says A's host holds A where it cannot take a line now (an unspent
///   usage-limit wall, a box only its owner can answer), as that host's last look read it
///   (`Wall`, `Past`): the seat rule then nominates it no more; its host, the first time it holds
///   A owed, returns it to the episode (`ReturnA`), and the second time releases it (`ReleaseA`),
///   so it stays elected; and a round with nobody else to elect waits for it to be past, while it
///   is not elected or has gone back, rather than running out (`ElectNone`). Only a round or an
///   owed line reads it, so it is kept there alone (phases 1 and 2) and dropped as the episode
///   leaves them; the next look there reads it again. B's and C's holds are not modeled: either
///   may go back to the episode at any owed step of theirs (`ReturnB`, `ReturnC`).
///
/// The rest record what a step saw, for the laws to judge: `sowed`, what the last line was
/// typed on; `scool` and `sdone`, what the last opening saw; `slate`, whether the last line went
/// after space was freed; `past`, whether the last episode closed past the margin of the floor
/// it was held to, or gave way a cooldown after it parked; `sflap`, whether an episode opened
/// after one that closed short of that; `skept`, whether A's host kept A owed with its task
/// done; `sfit`, whether the last line to A went while the seat rule admitted it; `swall`,
/// whether A's host kept A owed while it held it at a wall or a box; `swait`, whether a round ran
/// out while A, not yet elected, was held so; `sret`, whether one ran out while A, gone back to
/// the episode, was held so. `End`, `Expire`, `KeepA`, `TypeA` and `ElectNone`
/// record from their own state what their guards are meant to hold, and each of their mutants is
/// the same step under a weaker guard, so a guard weakened in the step itself breaks the law too.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn disk_free_ask_model() -> Model {
    aterm_spec::ty_model! {
        DiskFreeAsk {
            const Buggy = 0;
            var free = 4;
            var bar = 0;
            var done = 0;
            var dA = 0;
            var phase = 0;
            var aged = 0;
            var owner = 0;
            var pA = 0;
            var pB = 0;
            var pC = 0;
            var lA = 0;
            var lB = 0;
            var lC = 0;
            var asks = 0;
            var cool = 0;
            var rose = 0;
            var raised = 0;
            var past = 1;
            var sowed = 1;
            var scool = 0;
            var sdone = 1;
            var slate = 0;
            var sflap = 0;
            var sfit = 1;
            var skept = 0;
            var uA = 0;
            var wA = 0;
            var swall = 0;
            var swait = 0;
            var sret = 0;

            action Drop when (free > 0) { free = free - 1; }
            action Climb when (4 > free) { free = free + 1; }
            action Spend when (done == 0) { done = 1; }
            action Unspend when (done == 1) { done = 0; }
            action Finish when (dA == 0) { dA = 1; }
            action Unfinish when (dA == 1) { dA = 0; }
            action Wall when ((phase == 1 || phase == 2) && wA == 0) { wA = 1; }
            action Past when ((phase == 1 || phase == 2) && wA == 1) { wA = 0; }
            action Cools when (cool == 1) { cool = 0; }
            action Ages when (phase > 3 && aged == 0) { aged = 1; }
            action OpenH when (free <= 2 && phase == 0 && done == 1 && cool == 0) {
                phase = 1;
                bar = 2;
                scool = cool;
                sdone = done;
                sflap = if (past == 0) { 1 } else { sflap };
            }
            action OpenL when (free == 0 && phase == 0 && done == 1 && cool == 0) {
                phase = 1;
                bar = 1;
                scool = cool;
                sdone = done;
                sflap = if (past == 0) { 1 } else { sflap };
            }
            action RaiseH when (free <= 3 && phase > 0 && bar == 1) { bar = 2; }
            action ElectA when (free <= 3 && phase == 1 && (pA == 0 || pA == 2)) {
                phase = 2;
                owner = 1;
                pA = if (pA == 2) { 3 } else { 1 };
            }
            action ElectB when (free <= 3 && phase == 1 && pB == 0) {
                phase = 2;
                owner = 2;
                pB = 1;
            }
            action ElectC when (free <= 3 && phase == 1 && pC == 0) {
                phase = 2;
                owner = 3;
                pC = 1;
            }
            action ElectNone when (free <= 3 && phase == 1 && (wA == 0 || pA == 1)) {
                phase = 5;
                raised = 1;
                swait = if (wA == 1 && pA == 0) { 1 } else { 0 };
                sret = if (wA == 1 && pA == 2) { 1 } else { 0 };
                wA = 0;
                pA = if (pA == 2) { 0 } else { pA };
            }
            action KeepA when (phase == 2 && owner == 1 && dA == 0 && wA == 0) {
                owner = 1;
                skept = dA;
                swall = wA;
            }
            action ReleaseA when (
                phase == 2 && owner == 1 && (dA == 1 || (wA == 1 && pA == 3))
            ) {
                phase = 1;
                owner = 0;
                pA = 1;
            }
            action ReturnA when (
                phase == 2 && owner == 1 && dA == 0 && wA == 1 && pA == 1 && uA == 0
            ) {
                phase = 1;
                owner = 0;
                pA = 2;
            }
            action ReturnB when (phase == 2 && owner == 2) { phase = 1; owner = 0; pB = 0; }
            action ReturnC when (phase == 2 && owner == 3) { phase = 1; owner = 0; pC = 0; }
            action TypeA when (phase == 2 && owner == 1 && dA == 0 && uA == 0) {
                lA = lA + 1;
                asks = asks + 1;
                phase = 3;
                cool = 1;
                sowed = 1;
                slate = rose;
                sfit = if (dA == 0) { 1 } else { 0 };
                wA = 0;
                pA = 1;
            }
            action TypeALost when (phase == 2 && owner == 1 && dA == 0 && uA == 0) {
                lA = lA + 1;
                cool = 1;
                uA = 1;
                sowed = 1;
                slate = rose;
                sfit = if (dA == 0) { 1 } else { 0 };
            }
            action RecordA when (uA == 1 && phase == 2 && owner == 1) {
                asks = asks + 1;
                phase = 3;
                uA = 0;
                wA = 0;
                pA = 1;
            }
            action RecordBackA when (uA == 1 && (phase == 1 || phase == 5)) {
                asks = asks + 1;
                phase = 3;
                owner = 1;
                aged = 0;
                uA = 0;
                wA = 0;
            }
            action RecordOverA when (uA == 1 && phase == 2 && owner > 1 && asks == 0) {
                asks = asks + 1;
                uA = 0;
            }
            action RecordWithdrawA when (uA == 1 && phase == 2 && owner > 1 && asks > 0) {
                asks = asks + 1;
                phase = 3;
                owner = 1;
                uA = 0;
                wA = 0;
            }
            action TypeB when (phase == 2 && owner == 2 && uA == 0) {
                lB = lB + 1;
                asks = asks + 1;
                phase = 3;
                cool = 1;
                sowed = 1;
                slate = rose;
                wA = 0;
            }
            action TypeC when (phase == 2 && owner == 3 && uA == 0) {
                lC = lC + 1;
                asks = asks + 1;
                phase = 3;
                cool = 1;
                sowed = 1;
                slate = rose;
                wA = 0;
            }
            action Frees when (phase == 3 && rose == 0) { rose = 1; }
            action Freed when (free <= 3 && phase == 3 && rose == 1) {
                phase = 4;
                owner = 0;
                pA = if (pA == 2) { 0 } else { pA };
            }
            action HandOffOwed when (phase == 2) {
                phase = 1;
                owner = 0;
                pA = if (pA == 3) { 1 } else { pA };
            }
            action HandOffAsked when (free <= 3 && phase == 3 && rose == 0 && 2 > asks) {
                phase = 1;
                owner = 0;
            }
            action OutAsked when (free <= 3 && phase == 3 && rose == 0 && asks > 1) {
                phase = 5;
                owner = 0;
                raised = 1;
                pA = if (pA == 2) { 0 } else { pA };
            }
            action End when (phase > 0 && (free == 4 || (bar == 1 && free > 1))) {
                phase = 0;
                aged = 0;
                bar = 0;
                owner = 0;
                pA = 0;
                pB = 0;
                pC = 0;
                lA = 0;
                lB = 0;
                lC = 0;
                asks = 0;
                rose = 0;
                raised = 0;
                uA = 0;
                past = if (free == 4 || (bar == 1 && free > 1)) { 1 } else { 0 };
                wA = 0;
            }
            action Expire when (phase > 3 && aged == 1 && cool == 0 && done == 1 && free <= 2) {
                phase = 0;
                aged = 0;
                bar = 0;
                owner = 0;
                pA = 0;
                pB = 0;
                pC = 0;
                lA = 0;
                lB = 0;
                lC = 0;
                asks = 0;
                rose = 0;
                raised = 0;
                uA = 0;
                past = if (aged == 1 && cool == 0) { 1 } else { 0 };
            }

            action BlindTypeA when (
                Buggy == 1 && pA == 1 && lA == 0
                    && (phase <= 1 || phase > 2 || owner <= 0 || owner > 1)
            ) {
                lA = 1;
                cool = 1;
                sowed = 0;
                slate = rose;
            }
            action OpenInCooldown when (
                Buggy == 2 && free <= 2 && phase == 0 && done == 1 && cool == 1
            ) {
                phase = 1;
                bar = 2;
                scool = cool;
                sdone = done;
            }
            action OpenBeforeDone when (
                Buggy == 3 && free <= 2 && phase == 0 && done == 0 && cool == 0
            ) {
                phase = 1;
                bar = 2;
                scool = cool;
                sdone = done;
            }
            action HandOffPastTwo when (
                Buggy == 4 && free <= 3 && phase == 3 && rose == 0 && asks > 1
            ) {
                phase = 1;
                owner = 0;
            }
            action ElectPickedA when (Buggy == 5 && free <= 3 && phase == 1 && pA == 1) {
                phase = 2;
                owner = 1;
            }
            action HandOffAfterFreed when (
                Buggy == 6 && free <= 3 && phase == 3 && rose == 1 && 2 > asks
            ) {
                phase = 1;
                owner = 0;
            }
            action OutSilent when (
                Buggy == 7 && free <= 3 && phase == 3 && rose == 0 && asks > 1
            ) {
                phase = 5;
                owner = 0;
            }
            action EndAtOwnFloor when (Buggy == 8 && phase > 0 && free > 0) {
                phase = 0;
                aged = 0;
                bar = 0;
                owner = 0;
                pA = 0;
                pB = 0;
                pC = 0;
                lA = 0;
                lB = 0;
                lC = 0;
                asks = 0;
                rose = 0;
                raised = 0;
                uA = 0;
                past = if (free == 4 || (bar == 1 && free > 1)) { 1 } else { 0 };
                wA = 0;
            }
            action EndAtHeldFloor when (
                Buggy == 9 && phase > 0 && (free > 2 || (bar == 1 && free > 0))
            ) {
                phase = 0;
                aged = 0;
                bar = 0;
                owner = 0;
                pA = 0;
                pB = 0;
                pC = 0;
                lA = 0;
                lB = 0;
                lC = 0;
                asks = 0;
                rose = 0;
                raised = 0;
                uA = 0;
                past = if (free == 4 || (bar == 1 && free > 1)) { 1 } else { 0 };
                wA = 0;
            }
            action KeepDoneA when (Buggy == 10 && phase == 2 && owner == 1) {
                owner = 1;
                skept = dA;
            }
            action TypeDoneA when (Buggy == 11 && phase == 2 && owner == 1) {
                lA = lA + 1;
                asks = asks + 1;
                phase = 3;
                cool = 1;
                sowed = 1;
                slate = rose;
                sfit = if (dA == 0) { 1 } else { 0 };
                wA = 0;
                pA = 1;
            }
            action RecordLostA when (
                Buggy == 13 && uA > 0 && (phase <= 1 || phase > 2 || owner <= 0 || owner > 1)
            ) {
                uA = 0;
            }
            action RecordKeepPastTwoA when (
                Buggy == 14 && uA == 1 && phase == 2 && owner > 1 && asks > 0
            ) {
                asks = asks + 1;
                uA = 0;
            }
            action KeepHeldA when (Buggy == 15 && phase == 2 && owner == 1 && dA == 0) {
                owner = 1;
                skept = dA;
                swall = wA;
            }
            action OutWhileHeldA when (Buggy == 16 && free <= 3 && phase == 1) {
                phase = 5;
                raised = 1;
                swait = if (wA == 1 && pA == 0) { 1 } else { 0 };
                sret = if (wA == 1 && pA == 2) { 1 } else { 0 };
                wA = 0;
            }
            action OutWhileReturnedA when (
                Buggy == 17 && free <= 3 && phase == 1 && (wA == 0 || pA > 0)
            ) {
                phase = 5;
                raised = 1;
                swait = if (wA == 1 && pA == 0) { 1 } else { 0 };
                sret = if (wA == 1 && pA == 2) { 1 } else { 0 };
                wA = 0;
            }
            action FreedUnrisen when (Buggy == 18 && free <= 3 && phase == 3 && rose == 0) {
                phase = 4;
                owner = 0;
                pA = if (pA == 2) { 0 } else { pA };
            }
            action ExpireEarly when (
                Buggy == 12 && phase > 3 && cool == 0 && done == 1 && free <= 2
            ) {
                phase = 0;
                aged = 0;
                bar = 0;
                owner = 0;
                pA = 0;
                pB = 0;
                pC = 0;
                lA = 0;
                lB = 0;
                lC = 0;
                asks = 0;
                rose = 0;
                raised = 0;
                uA = 0;
                past = if (aged == 1 && cool == 0) { 1 } else { 0 };
            }

            invariant TypedOnlyWhileOwed: sowed == 1;
            invariant NoOpenInCooldown: phase == 0 || scool == 0;
            invariant OpenedOnlyDone: phase == 0 || sdone == 1;
            invariant AtMostTwoLines: lA + lB + lC <= 2;
            invariant OneLinePerSession: lA <= 1 && lB <= 1 && lC <= 1;
            invariant NoAskAfterFreed: slate == 0;
            invariant OutIsRaised: phase <= 4 || raised == 1;
            invariant NoFlap: sflap == 0;
            invariant DoneIsReleased: skept == 0;
            invariant TypedOnlyFit: sfit == 1;
            invariant LinesCounted: uA > 0 || asks == lA + lB + lC;
            invariant HeldIsReleased: swall == 0;
            invariant HeldIsWaitedFor: swait == 0;
            invariant ReturnedIsWaitedFor: sret == 0;
            invariant FreedOnlyByAgent: phase <= 3 || phase > 4 || rose == 1;
            bound PhaseBox: phase <= 5;
            bound AsksBox: asks <= 3;
            bound FreeBox: free <= 4;
            bound BarBox: bar <= 2;
            bound LostBox: uA <= 1;
        }
    }
}
