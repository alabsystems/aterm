// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models for the window's harness host, lease handoff, and session lifecycle.
use aterm_spec::derive::Model;

/// Two restored tabs in the host's one retry queue.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restored_first_attempt_model() -> Model {
    aterm_spec::ty_model! {
        HarnessRestoredFirstAttempt {
            const Buggy = 0;
            var a_first = 0;
            var b_first = 0;
            var a_retry = 0;

            action FirstA when (a_first == 0) {
                a_first = 1;
            }
            action FirstB when (a_first == 1 && b_first == 0) {
                b_first = 1;
            }
            action RetryA when (a_first == 1 && a_retry == 0 && (b_first == 1 || Buggy == 1)) {
                a_retry = 1;
            }

            invariant FirstAttemptsBeforeRetry: a_retry == 0 || b_first == 1;
        }
    }
}

/// Harness work in flight at a seamless update's Commit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restored_carry_model() -> Model {
    aterm_spec::ty_model! {
        HarnessRestoredCarry {
            const Buggy = 0;
            var queued = 1;
            var acting = 0;
            var typed = 0;
            var ran = 0;
            var overtaken = 0;
            var paused = 0;
            var captured = 0;
            var capture = 0;
            var committed = 0;
            var succ = 0;
            var resolved = 0;
            var late = 0;
            var over = 0;
            var restart = 1;
            var swept = 0;

            action StepStart when (
                queued == 1 && acting == 0 && overtaken == 0 && paused == 0 && committed == 0
            ) {
                acting = 1;
                over = if (ran == 1 && typed == 0) { 1 } else { over };
            }
            action StepStartWhileParked when (
                Buggy == 1 && queued == 1 && acting == 0 && overtaken == 0 && paused == 1
                    && committed == 0
            ) {
                acting = 1;
                late = 1;
            }
            action StepStartOvertaken when (
                Buggy == 1 && queued == 1 && acting == 0 && overtaken == 1 && paused == 0
                    && committed == 0
            ) {
                acting = 1;
                over = 1;
            }
            action StepRetries when (acting == 1 && committed == 0) {
                acting = 0;
            }
            action StepTypes when (acting == 1 && committed == 0) {
                acting = 0;
                typed = 1;
                overtaken = 0;
            }
            action StepResolves when (acting == 1 && committed == 0) {
                acting = 0;
                queued = 0;
                resolved = 1;
            }
            action AgentRuns when (queued == 1 && overtaken == 0 && committed == 0) {
                ran = 1;
                overtaken = if (typed == 1) { 0 } else { 1 };
            }
            action AgentStopsForgotten when (Buggy == 1 && overtaken == 1 && committed == 0) {
                overtaken = 0;
            }
            action Overtaken when (
                queued == 1 && acting == 0 && overtaken == 1 && paused == 0 && committed == 0
            ) {
                queued = 0;
            }
            action RestartFinishes when (restart == 1 && committed == 0) {
                restart = 0;
            }
            action Park when (paused == 0 && committed == 0) {
                paused = 1;
                captured = if (queued == 1 && overtaken == 0) { 1 } else { 0 };
                capture = 0;
            }
            action ParkDropsQueue when (Buggy == 1 && paused == 0 && committed == 0) {
                paused = 1;
                captured = 0;
                capture = 1;
            }
            action ParkFreezes when (Buggy == 1 && paused == 0 && committed == 0) {
                paused = 1;
                captured = queued;
                capture = 2;
            }
            action Rollback when (paused == 1 && committed == 0) {
                paused = 0;
                captured = 0;
                capture = 0;
            }
            action Commit when (
                paused == 1 && committed == 0
                    && (capture == 2
                        || (capture == 1 && captured == 0)
                        || (capture == 0
                            && ((captured == 1 && queued == 1 && overtaken == 0)
                                || (captured == 0 && (queued == 0 || overtaken == 1)))))
            ) {
                committed = 1;
                succ = captured;
                queued = 0;
                acting = 0;
                swept = restart;
            }
            action CommitWithoutSweep when (
                Buggy == 1 && paused == 1 && committed == 0
                    && (capture == 2
                        || (capture == 1 && captured == 0)
                        || (capture == 0
                            && ((captured == 1 && queued == 1 && overtaken == 0)
                                || (captured == 0 && (queued == 0 || overtaken == 1)))))
            ) {
                committed = 1;
                succ = captured;
                queued = 0;
                acting = 0;
                swept = 0;
            }
            action SuccRelaunches when (committed == 1 && succ == 1) {
                succ = 0;
                resolved = 1;
                over = if (ran == 1 && typed == 0) { 1 } else { over };
            }
            action SweepCarriesOn when (committed == 1 && swept == 1) {
                swept = 0;
                restart = 0;
            }

            invariant NoSilentLoss: committed == 0 || resolved == 1 || succ == 1 || ran == 1;
            invariant NoStepWhileParked: late == 0;
            invariant NoRelaunchOverAgent: over == 0;
            invariant NoStrandedRestart: committed == 0 || restart == 0 || swept == 1;
        }
    }
}

/// A crashed agent's relaunch through a park's Commit that fails.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_commit_owed_model() -> Model {
    aterm_spec::ty_model! {
        HarnessCommitOwed {
            const Buggy = 0;
            var agent = 1;
            var worker = 1;
            var stop = 0;
            var handover = 0;
            var commit = 0;
            var unwatched = 0;
            var phase = 0;
            var done = 0;
            var queued = 0;
            var stale = 0;
            var captured = 0;
            var succ = 0;
            var dropped = 0;
            var firstfound = 0;
            var blind = 0;
            var closed = 0;
            var down = 0;
            var seen = 0;
            var swept = 0;
            var landed = 0;
            var qlanded = 0;

            action Crash when (
                agent == 1
                    && (worker > 0 || unwatched == 1 || landed == 1 || qlanded == 1)
                    && (queued == 0 || qlanded == 1) && closed == 0
            ) {
                agent = 0;
                done = 0;
                seen = 1;
                swept = 0;
                qlanded = 0;
            }
            action CrashUnseen when (
                Buggy == 1 && agent == 1 && phase == 1
                    && (unwatched == 1 || (worker == 1 && stop == 1)) && queued == 0
                    && closed == 0
            ) {
                agent = 0;
                done = 0;
                seen = 0;
                swept = 0;
            }
            action LandedLost when (
                Buggy == 1 && agent == 1 && landed == 1 && queued == 0 && closed == 0
            ) {
                agent = 0;
                done = 0;
                landed = 0;
                seen = 0;
                swept = 0;
            }
            action Sweep when (agent == 0 && swept == 0) {
                swept = 1;
            }
            action Back when (agent == 0 && down == 0) {
                agent = 1;
                unwatched = if (worker > 1 && stop == 0) { 1 } else { unwatched };
            }
            action Settle when (worker > 1 && agent == 1) {
                worker = 0;
                unwatched = if (stop == 1 && (commit == 1 || commit == 3)) { 1 } else { unwatched };
                commit = 0;
            }
            action SettleGone when (
                Buggy == 1 && worker > 1 && agent == 0 && (unwatched == 1 || stop == 1)
            ) {
                worker = 0;
                unwatched = if (stop == 1 && (commit == 1 || commit == 3)) { 1 } else { unwatched };
                blind = if (stop == 1 && (commit == 1 || commit == 3)) { 1 } else { unwatched };
                commit = 0;
            }
            action Rearm when (worker > 1 && agent == 0 && unwatched == 1) {
                unwatched = 0;
            }
            action GoneReadAsStop when (
                Buggy == 1 && worker > 1 && agent == 0 && unwatched == 1 && stop == 0
            ) {
                worker = 0;
                commit = 0;
                unwatched = 0;
            }
            action Hand when (
                agent == 0 && worker == 1 && stop == 0 && (phase == 0 || phase > 1) && down == 0
            ) {
                worker = 2;
            }
            action Read when (worker == 2) {
                worker = if (seen == 1 || swept == 0) { 3 } else { 0 };
            }
            action StopEndsUnread when (Buggy == 1 && worker == 2 && stop == 1) {
                worker = 0;
            }
            action Relaunch when (worker == 3 && stop == 0 && (phase == 0 || phase == 3)) {
                worker = 0;
                agent = 1;
                done = 1;
                landed = 1;
            }
            action RelaunchUnread when (
                worker == 3 && stop == 0 && agent == 0 && (phase == 0 || phase == 3)
            ) {
                agent = 1;
                done = 1;
            }
            action UnreadLandedLost when (
                Buggy == 1 && worker == 3 && stop == 0 && agent == 0 && (phase == 0 || phase == 3)
            ) {
                worker = 0;
                done = 0;
            }
            action HandOver when (agent == 1 && worker == 1 && stop == 0) {
                stop = 1;
                handover = 1;
            }
            action Suspend when (phase == 0) {
                phase = 1;
                stop = if (worker > 0) { 1 } else { stop };
                commit = if (worker > 0 && stop == 0) { 1 } else { commit };
            }
            action Decide when (worker == 3 && stop == 1) {
                worker = 0;
                commit = if (commit == 1 || (commit == 3 && closed == 1)) { 2 } else { 0 };
                queued = if (commit == 3 && closed == 0) { 1 } else { queued };
            }
            action DecideClosedDrops when (
                Buggy == 1 && worker == 3 && stop == 1 && commit == 3 && closed == 1
            ) {
                worker = 0;
                commit = 0;
            }
            action DecideByPark when (Buggy == 1 && worker == 3 && stop == 1 && phase == 3) {
                worker = 0;
                commit = 0;
            }
            action EndUnhanded when (worker == 1 && stop == 1) {
                worker = 0;
                handover = 0;
                unwatched = if (commit == 1 || commit == 3 || handover == 1) { 1 } else { unwatched };
            }
            action EndUnhandedForgotten when (Buggy == 1 && worker == 1 && stop == 1) {
                worker = 0;
                handover = 0;
            }
            action EndHandedOverForgotten when (
                Buggy == 1 && worker == 1 && stop == 1 && handover == 1
            ) {
                worker = 0;
                handover = 0;
                unwatched = if (commit == 1 || commit == 3) { 1 } else { unwatched };
            }
            action Resume when (phase == 1) {
                phase = 2;
            }
            action Rollback when (phase == 2) {
                phase = 3;
                commit = if (commit == 1) { 3 } else { if (commit == 2) { 0 } else { commit } };
                queued = if (commit == 2) { 1 } else { queued };
            }
            action HandUnwatched when (
                worker == 0 && agent == 0 && (unwatched == 1 || (landed == 1 && seen == 1))
                    && (phase == 0 || phase > 1) && closed == 0 && down == 0
            ) {
                worker = if (blind == 1) { 0 } else { 2 };
                stop = 0;
                handover = 0;
                commit = 0;
                unwatched = 0;
                blind = 0;
                landed = 0;
            }
            action Start when (
                worker == 0 && agent == 1 && (phase == 0 || phase > 1) && closed == 0
                    && down == 0
            ) {
                worker = 1;
                stop = 0;
                handover = 0;
                commit = 0;
                unwatched = 0;
                blind = 0;
                landed = 0;
                queued = if (qlanded == 1) { 0 } else { queued };
                qlanded = 0;
            }
            action QueueSteps when (queued == 1 && qlanded == 0 && phase == 3 && closed == 0) {
                queued = 0;
                agent = 1;
                done = 1;
                landed = 1;
            }
            action QueueStepsUnread when (
                queued == 1 && qlanded == 0 && phase == 3 && closed == 0
            ) {
                agent = 1;
                done = 1;
                qlanded = 1;
            }
            action QueueReadsLanded when (
                qlanded == 1 && agent == 1 && phase == 3 && closed == 0
            ) {
                queued = 0;
                qlanded = 0;
                landed = 1;
            }
            action QueueLandedLost when (
                Buggy == 1 && queued == 1 && qlanded == 0 && phase == 3 && closed == 0
            ) {
                queued = 0;
                done = 0;
            }
            action QueueUnreadLost when (Buggy == 1 && qlanded == 1 && agent == 1 && closed == 0) {
                agent = 0;
                done = 0;
                queued = 0;
                qlanded = 0;
            }
            action Overtook when (phase == 0 && stale == 0) {
                stale = 1;
            }
            action Park when (phase == 3 && closed == 0) {
                phase = 4;
                captured = queued;
            }
            action ParkFirstFound when (Buggy == 1 && phase == 3 && closed == 0) {
                phase = 4;
                captured = if (stale == 1) { 0 } else { queued };
                firstfound = 1;
            }
            action Quit when (phase == 3 && commit == 3 && closed == 0) {
                closed = 1;
            }
            action Panic when (down == 0 && closed == 0) {
                down = 1;
            }
            action PanicStops when (
                Buggy == 1 && down == 0 && closed == 0 && worker > 0 && stop == 0
            ) {
                down = 1;
                stop = 1;
            }
            action RestartPanicStops when (
                Buggy == 1 && down == 1 && closed == 0 && worker > 0 && stop == 0
            ) {
                stop = 1;
            }
            action Up when (down == 1) {
                down = 0;
            }
            action Commit when (
                phase == 4
                    && ((firstfound == 0 && captured == queued)
                        || (firstfound == 1 && stale == 1 && captured == 0)
                        || (firstfound == 1 && stale == 0 && captured == queued))
            ) {
                phase = 5;
                succ = captured;
                dropped = if (queued == 1 && captured == 0) { 1 } else { 0 };
                queued = 0;
            }

            invariant NoOwedRelaunchLost: phase <= 2 || agent == 1 || done == 1 || queued == 1
                || succ == 1 || worker > 0 || unwatched == 1 || commit == 2 || landed == 1;
            invariant NoQueuedRelaunchDropped: dropped == 0;
        }
    }
}

/// `aterm-gui`'s `harness_host` runs one worker per agent session.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_worker_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        HarnessWorkerLifecycle {
            const Buggy = 0;
            const Budget = 5;
            var wanted = 0;
            var cur = 0;
            var old = 0;
            var faults = 0;
            var faulted = 0;
            var held = 0;
            var on = 1;
            var off = 0;
            var letgo = 0;
            var vendor = 0;

            action Arrive when (wanted == 0) {
                wanted = 1;
            }
            action ArriveVendor when (wanted == 0) {
                wanted = 1;
                vendor = 1;
            }
            action Leave when (wanted == 1 && old + cur <= 1) {
                wanted = 0;
                vendor = 0;
                old = old + cur;
                cur = 0;
                held = 0;
            }
            action SwitchOff when (on == 1 && old + cur <= 1) {
                on = 0;
                old = old + cur;
                cur = 0;
                held = 0;
            }
            action SwitchOn when (on == 0) {
                on = 1;
            }
            action Start when (wanted == 1 && cur == 0
                && ((on == 1 && held == 0 && old == 0 && off == 0 && vendor == 0) || Buggy == 1)) {
                cur = 1;
            }
            action Exit when (old > 0) {
                old = old - 1;
                faulted = 0;
            }
            action Fail when (cur == 1 && faults <= Budget + 1) {
                faults = if (faults + 1 > Budget + 1 && Buggy == 0) { Budget + 1 } else { faults + 1 };
                faulted = if (faults + 1 > Budget) { 1 } else { 0 };
                cur = if (faults + 1 > Budget && Buggy == 1) { 0 } else { 1 };
            }
            action Restart when (cur == 1 && faulted == 1) {
                faulted = 0;
            }
            action Age when (faults > 0) {
                faults = faults - 1;
            }
            action Hold when (cur == 1) {
                cur = 0;
                held = 1;
                faulted = 0;
            }
            action Release when (held == 1) {
                held = 0;
            }
            action Reload when (old + cur <= 1) {
                old = old + cur;
                cur = 0;
                faults = 0;
            }
            action TurnOff when (off == 0 && old + cur <= 1) {
                off = 1;
                letgo = if (cur == 1 && Buggy == 1) { 1 } else { letgo };
                old = old + cur;
                cur = 0;
            }
            action TurnOn when (off == 1) {
                off = 0;
            }
            action Crash when (wanted == 1 && old + cur <= 1) {
                wanted = 0;
                vendor = 0;
                held = 0;
                letgo = if (cur == 1 && off == 1) { 1 } else { letgo };
                old = old + cur;
                cur = 0;
            }

            invariant OneSupervisor: old + cur <= 1;
            invariant FaultBudget: faults <= Budget + 1;
            invariant NeverGivesUp: faulted == 0 || cur == 1 || old > 0;
            invariant HeldIsOff: held == 0 || cur == 0;
            invariant OffMeansNoCurrent: on == 1 || cur == 0;
            invariant OffStopsTheWorker: off == 0 || cur == 0;
            invariant OffLetsNothingGo: letgo == 0;
            invariant NoWorkerForAVendorCodex: vendor == 0 || cur == 0;
        }
    }
}

/// One session's supervisor claim across a seamless update.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_handoff_claim_model() -> Model {
    aterm_spec::ty_model! {
        HarnessHandoffClaim {
            const Buggy = 0;
            const Renew = 2;
            const Grace = 3;
            var phase = 0;
            var ext = 0;
            var own = 0;
            var older = 0;
            var server = 0;
            var grace = 0;
            var wait = 0;
            var renewed = 0;
            var took = 0;

            action ExternalLease when (phase == 0 && ext == 0 && own == 0) {
                ext = 1;
            }
            action ExternalConn when (phase == 0 && ext == 0 && own == 0) {
                ext = 2;
            }
            action HostHeld when (phase == 0 && ext == 0 && own == 0) {
                own = 1;
            }
            action OlderParent when (phase == 0 && older == 0) {
                older = 1;
            }
            action Park when (phase == 0) {
                phase = 1;
            }
            action Commit when (phase == 1) {
                phase = 2;
                server = if (older == 0 && ext == 1) { 1 } else { 0 };
                grace = if (older == 1 || ext > 0) { Grace } else { 0 };
            }
            action CommitUncarried when (Buggy == 1 && phase == 1) {
                phase = 2;
                server = 0;
                grace = 0;
            }
            action CommitOwnLease when (Buggy == 1 && phase == 1 && older == 0) {
                phase = 2;
                server = if (ext == 1 || own == 1) { 1 } else { 0 };
                grace = if (ext > 0) { Grace } else { 0 };
            }
            action CommitGraceAll when (Buggy == 1 && phase == 1) {
                phase = 2;
                server = if (older == 0 && ext == 1) { 1 } else { 0 };
                grace = Grace;
            }
            action Tick when (phase == 2 && (grace > 0 || (ext > 0 && renewed == 0))
                && (ext == 0 || renewed == 1 || wait <= Renew - 1)) {
                grace = if (grace > 0) { grace - 1 } else { 0 };
                wait = if (wait <= Renew - 1) { wait + 1 } else { Renew };
            }
            action Lapse when (phase == 2 && server == 1 && renewed == 0) {
                server = 0;
            }
            action ExternalRenews when (phase == 2 && ext > 0 && renewed == 0) {
                renewed = 1;
                server = if (server == 2) { 2 } else { 1 };
            }
            action HostClaims when (phase == 2 && server == 0 && grace == 0) {
                server = 2;
                took = if (ext > 0 && renewed == 0) { 1 } else { took };
            }

            invariant NoTakeover: took == 0;
            invariant NoPhantomClaim: ext > 0 || server == 0 || server == 2;
            invariant NoNeedlessGrace: grace == 0 || older == 1 || ext > 0;
        }
    }
}

/// The host hands a worker its agent's exit only for an exit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_leave_model() -> Model {
    aterm_spec::ty_model! {
        HarnessLeave {
            const Buggy = 0;
            var alive = 1;
            var named = 1;
            var ours = 0;
            var stray = 0;
            var due = 0;
            var handed = 0;
            var decided = 0;
            var off = 0;

            action Misread when (alive == 1 && named == 1) {
                named = 0;
                due = 1;
            }
            action Reread when (alive == 1 && named == 0) {
                named = 1;
                due = 1;
            }
            action Exit when (alive == 1) {
                alive = 0;
                named = 0;
                due = if (named == 1) { 1 } else { due };
            }
            action Terminate when (alive == 1) {
                alive = 0;
                named = 0;
                ours = 1;
                due = 1;
            }
            action Stray when (alive == 1 && stray == 0) {
                stray = 1;
            }
            action SwitchOff when (off == 0 && handed == 0) {
                off = 1;
                due = 1;
            }
            action Visit when (due == 1 && handed == 0) {
                due = if (named == 0 && alive == 1 && off == 0) { 1 } else { 0 };
                handed = if (named == 0 && alive == 0 && off == 0) { 1 } else { 0 };
            }
            action VisitUnconfirmed when (Buggy == 1 && due == 1 && handed == 0) {
                due = 0;
                handed = if (named == 0) { 1 } else { 0 };
            }
            action VisitNoLook when (Buggy == 1 && due == 1 && handed == 0) {
                due = 0;
                handed = if (named == 0 && alive == 0) { 1 } else { 0 };
            }
            action VisitOffAsLeave when (Buggy == 1 && due == 1 && handed == 0 && off == 1) {
                due = 0;
                handed = 1;
            }
            action Decide when (handed == 1 && decided == 0) {
                decided = if (ours == 1) { 1 } else { 2 };
            }
            action DecideLimited when (Buggy == 1 && handed == 1 && decided == 0) {
                decided = 2;
            }
            action DecideByTab when (Buggy == 1 && handed == 1 && decided == 0) {
                decided = if (ours == 1 || stray == 1) { 1 } else { 2 };
            }

            invariant NoExitWhileAlive: handed == 0 || alive == 0;
            invariant NoExitWhileOff: handed == 0 || off == 0;
            invariant NeverMissed: alive == 1 || handed == 1 || due == 1 || off == 1;
            invariant TheUpgradesOwnIsCarried: ours == 0 || decided <= 1;
            invariant OnlyTheUpgradesOwnIsCarried: ours == 1 || decided == 0 || decided == 2;
        }
    }
}

/// `aterm-agent`'s `harness::relaunch` as the host drives it, for ONE session's agent.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_relaunch_on_exit_model() -> Model {
    aterm_spec::ty_model! {
        HarnessRelaunchOnExit {
            const Buggy = 0;
            const SaidOutlives = 0;
            const Steps = 3;
            const Badge = 3;
            var running = 1;
            var asked = 0;
            var pending = 0;
            var badged = 0;
            var step = 0;
            var misses = 0;
            var restarted = 0;
            var crash = 0;
            var streak = 0;
            var base = 0;
            var asks = 0;
            var failing = 0;
            var said = 0;

            action Restarted when (running == 1) {
                running = 0;
                pending = 1;
                asked = 0;
                restarted = 1;
                crash = 0;
                base = streak;
                step = 0;
                failing = 0;
                said = 0;
            }
            action Crash when (running == 1) {
                running = 0;
                pending = 1;
                asked = 0;
                restarted = 0;
                crash = 1;
                base = streak;
                step = streak;
                failing = 0;
                said = 0;
            }
            action Asked when (running == 1) {
                running = 0;
                pending = 1;
                asked = 0;
                restarted = 0;
                crash = 0;
                base = streak;
                step = 0;
                failing = 0;
                said = 0;
            }
            action Lapse when (pending == 1 && crash == 0 && restarted == 0) {
                crash = 1;
                step = if (streak + step <= Steps) { streak + step } else { Steps };
            }
            action PersonExit when (running == 1) {
                running = 0;
                asked = 1;
                pending = if (Buggy == 1) { 1 } else { 0 };
                restarted = 0;
                crash = 1;
                base = streak;
                step = streak;
                failing = 0;
                said = 0;
            }
            action Limited when (running == 1 || (pending == 1 && (restarted == 0 || Buggy == 1))) {
                running = 0;
                pending = 0;
                badged = if (Buggy == 1) { badged } else { 1 };
                restarted = if (running == 1) { 0 } else { restarted };
                crash = if (running == 1) { 1 } else { crash };
                base = if (running == 1) { streak } else { base };
                step = if (running == 1) { streak } else { step };
                asks = if (failing == 1 && Buggy == 0) { 0 } else { asks };
                failing = if (Buggy == 1) { failing } else { 0 };
                said = 0;
            }
            action PersonRuns when (running == 0 && pending == 0) {
                running = 1;
                asked = 0;
                badged = if (asks == 1 && Buggy == 0) { badged } else { 0 };
                restarted = 0;
                step = if (crash == 1) { streak } else { 0 };
                failing = 0;
                said = if (SaidOutlives == 1) { said } else { 0 };
            }
            action Ask when (running == 0 && badged == 1 && asks == 0) {
                asks = 1;
            }
            action AskedRuns when (running == 1 && asks == 1) {
                asks = 0;
                badged = 0;
                failing = 0;
                said = 0;
            }
            action Land when (pending == 1) {
                running = 1;
                pending = 0;
                badged = 0;
                misses = 0;
                streak = if ((crash == 1 || Buggy == 1) && streak <= Steps - 1) { streak + 1 } else { streak };
                step = if (crash == 0 && Buggy == 0) { 0 } else if (streak <= Steps - 1) { streak + 1 } else { streak };
                restarted = 0;
                asks = 0;
                failing = 0;
                said = 0;
            }
            action Miss when (pending == 1) {
                misses = if (misses <= Badge - 1) { misses + 1 } else { Badge };
                badged = if (misses + 1 > Badge - 1) { 1 } else { badged };
                failing = if (misses + 1 > Badge - 1 && badged == 0) { 1 } else { failing };
                said = if (misses == Badge - 1 && badged == 1 && failing == 0 && asks == 1) { 1 } else { said };
                step = if (step <= Steps - 1) { step + 1 } else { Steps };
                streak = if (crash == 1 && streak <= Steps - 1) { streak + 1 } else { streak };
            }
            action Ended when (pending == 1 && (restarted == 0 || Buggy == 1)) {
                pending = 0;
                asked = 1;
                badged = if (failing == 1 && Buggy == 0) { 0 } else { badged };
                asks = if (failing == 1 && Buggy == 0) { 0 } else { asks };
                failing = if (Buggy == 1) { failing } else { 0 };
                said = if (SaidOutlives == 1) { said } else { 0 };
            }
            action Cannot when (pending == 1) {
                pending = 0;
                badged = if (Buggy == 1) { badged } else { 1 };
                asks = if (failing == 1 && Buggy == 0) { 0 } else { asks };
                failing = if (Buggy == 1) { failing } else { 0 };
                said = 0;
            }
            action Healthy when (running == 1 && streak > 0) {
                step = 0;
                streak = 0;
                base = 0;
            }
            action PersonReturns when (pending == 1 && (restarted == 0 || Buggy == 1)) {
                pending = 0;
                asked = 1;
                badged = if (failing == 1 && Buggy == 0) { 0 } else { badged };
                asks = if (failing == 1 && Buggy == 0) { 0 } else { asks };
                failing = if (Buggy == 1) { failing } else { 0 };
                said = if (SaidOutlives == 1) { said } else { 0 };
            }

            invariant PersonWins: asked == 0 || pending == 0;
            invariant NeverSilent: running == 1 || pending == 1 || badged == 1 || asked == 1;
            invariant RestartCarried: restarted == 0 || asked == 0;
            invariant NoCrashNoStreak: crash == 1 || streak == base;
            invariant AskStandsForItsConversation: asks == 0 || badged == 1;
            invariant TriesOnlyWhileDue: (failing == 0 && said == 0) || pending == 1;
        }
    }
}
/// What an exit left, read as it is seen.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_exit_record_model() -> Model {
    aterm_spec::ty_model! {
        HarnessExitRecord {
            const Buggy = 0;
            var exit = 0;
            var record = 1;
            var seen = 0;
            var kept = 0;
            var decided = 0;

            action Crash when (exit == 0) {
                exit = 1;
            }
            action Graceful when (exit == 0) {
                exit = 2;
            }
            action OwnRemoval when (exit == 2 && record == 1) {
                record = 0;
            }
            action Look when (exit > 0 && seen == 0 && (exit == 1 || record == 0)) {
                seen = 1;
                kept = record;
            }
            action LookAtInstant when (Buggy == 1 && exit > 0 && seen == 0) {
                seen = 1;
                kept = record;
            }
            action Sweep when (exit > 0 && seen == 1 && record == 1) {
                record = 0;
            }
            action Decide when (seen == 1 && decided == 0 && (exit == 1 || record == 0)) {
                decided = if (kept == 1) { 1 } else { 2 };
            }
            action DecideAtAttempt when (Buggy == 1 && seen == 1 && decided == 0 && (exit == 1 || record == 0)) {
                decided = if (record == 1) { 1 } else { 2 };
            }

            invariant CrashIsRelaunched: exit == 0 || exit == 2 || decided <= 1;
            invariant GracefulIsLeft: exit <= 1 || decided == 0 || decided == 2;
        }
    }
}

/// A MUSE CODE EXIT.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_muse_exit_witness_model() -> Model {
    aterm_spec::ty_model! {
        HarnessMuseExitWitness {
            const Buggy = 0;
            var exit = 0;
            var witness = 1;
            var word = 0;
            var later = 0;
            var seen = 0;
            var kept = 0;
            var told = 0;
            var decided = 0;

            action Crash when (exit == 0) {
                exit = 1;
            }
            action OwnExit when (exit == 0) {
                exit = 2;
            }
            action Signal when (exit == 0) {
                exit = 3;
            }
            action Refuse when (exit == 0) {
                exit = 4;
            }
            action Unwitnessed when (exit == 0 && witness == 1) {
                witness = 0;
            }
            action ShellWord when (exit > 0 && word == 0) {
                word = 1;
            }
            action Later when (word == 1 && later == 0) {
                later = 1;
            }
            action Look when (exit > 0 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = if exit == 1 { 1 } else { if exit == 4 { 4 } else { 2 } };
            }
            action LookGone when (exit > 0 && seen == 0 && (word == 0 || later == 1)) {
                seen = 1;
                kept = 3;
            }
            action LookAsClaude when (Buggy == 1 && exit > 0 && seen == 0 && word == 0) {
                seen = 1;
                kept = if exit == 2 { 2 } else { 1 };
            }
            action RefusalAsCrash when (Buggy == 1 && exit > 0 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = if (exit == 2 || exit == 3) { 2 } else { 1 };
            }
            action Decide when (seen == 1 && decided == 0) {
                told = if (kept <= 2 || kept == 4 || (word == 1 && later == 0)) { 1 } else { 0 };
                decided = if (witness == 1
                    && (kept == 1
                        || (kept == 3 && word == 1 && later == 0 && exit == 1)
                        || (kept == 3 && (word == 0 || later == 1) && (exit == 1 || exit == 4))))
                {
                    1
                } else {
                    2
                };
            }
            action DecideOnLook when (Buggy == 1 && seen == 1 && decided == 0) {
                told = if (kept <= 2 || kept == 4) { 1 } else { 0 };
                decided = if (witness == 1 && kept == 1) { 1 } else { 2 };
            }
            action DecideFresh when (Buggy == 1 && seen == 1 && decided == 0 && witness == 0) {
                told = if (kept <= 2 || kept == 4 || (word == 1 && later == 0)) { 1 } else { 0 };
                decided = if (kept == 1 || (kept == 3 && word == 1 && later == 0 && exit == 1)) {
                    1
                } else {
                    2
                };
            }

            invariant TheirsIsLeft: exit <= 1 || exit == 4 || decided == 0 || decided == 2;
            invariant AToldRefusalIsNeverRetried: exit <= 3 || told == 0 || decided == 0 || decided == 2;
            invariant AWitnessedCrashIsRelaunched: exit == 0 || exit > 1 || witness == 0 || decided <= 1;
            invariant NoWitnessNoRelaunch: witness == 1 || decided == 0 || decided == 2;
        }
    }
}

/// MUSE CODE'S LIVE UPGRADE.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_muse_quiet_upgrade_model() -> Model {
    aterm_spec::ty_model! {
        HarnessMuseQuietUpgrade {
            const Buggy = 0;
            var landed = 0;
            var turn = 0;
            var task = 0;
            var queued = 0;
            var child = 0;
            var shown = 0;
            var draft = 0;
            var held = 0;
            var unread = 0;
            var moved = 0;
            var claimed = 0;
            var exited = 0;
            var decided = 0;
            var stranded = 0;
            var at_turn = 0;
            var at_task = 0;
            var at_queued = 0;
            var at_child = 0;
            var slipped = 0;

            action LandNewer when (moved == 0 && landed == 0) {
                landed = 1;
            }
            action LandOlder when (moved == 0 && landed == 0) {
                landed = 2;
            }
            action TurnStarts when (moved == 0 && turn == 0) {
                turn = 1;
                shown = 1;
            }
            action TurnEnds when (moved == 0 && turn == 1) {
                turn = 0;
            }
            action ScreenBusy when (moved == 0 && shown == 0) {
                shown = 1;
            }
            action ScreenIdle when (moved == 0 && shown == 1) {
                shown = 0;
            }
            action Backgrounded when (moved == 0 && turn == 1 && task == 0) {
                task = 1;
            }
            action WorkEnds when (moved == 0 && task == 1) {
                task = 0;
                queued = 1;
            }
            action Drained when (moved == 0 && queued == 1 && turn == 0) {
                queued = 0;
                turn = 1;
                shown = 1;
            }
            action ChildUp when (moved == 0 && child == 0) {
                child = 1;
            }
            action ChildGone when (moved == 0 && child == 1) {
                child = 0;
            }
            action DraftTyped when (moved == 0 && draft == 0) {
                draft = 1;
            }
            action DraftCleared when (moved == 0 && draft == 1) {
                draft = 0;
            }
            action Hold when (moved == 0 && held == 0) {
                held = 1;
            }
            action Release when (moved == 0 && held == 1) {
                held = 0;
            }
            action LogUnread when (moved == 0 && unread == 0) {
                unread = 1;
            }
            action LogRead when (moved == 0 && unread == 1) {
                unread = 0;
            }
            action Restart when (
                moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && queued == 0 && child == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0
            ) {
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_queued = queued;
                at_child = child;
            }
            action Exit when (moved == 1 && exited == 0) {
                exited = 1;
            }
            action ReadExit when (exited == 1 && decided == 0) {
                decided = if claimed == 1 { 1 } else { 2 };
            }
            action RestartWithOpenWork when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && child == 0 && shown == 0 && draft == 0 && held == 0 && unread == 0 &&
                (task == 1 || queued == 1)
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_queued = queued;
                at_child = child;
            }
            action RestartMidTurn when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 1 && task == 0 && queued == 0 && child == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_queued = queued;
                at_child = child;
            }
            action RestartWithChild when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && queued == 0 && child == 1 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_queued = queued;
                at_child = child;
            }
            action RestartUnclaimed when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && queued == 0 && child == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 0;
                at_turn = turn;
                at_task = task;
                at_queued = queued;
                at_child = child;
            }
            action RevisionReadAsOlder when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && queued == 0 && child == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0
            ) {
                slipped = 1;
                stranded = 1;
            }
            action RevisionReadAsNewer when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 2 &&
                turn == 0 && task == 0 && queued == 0 && child == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_queued = queued;
                at_child = child;
            }

            invariant NoRunningWorkInterrupted: at_task == 0 && at_queued == 0;
            invariant NeverMidTurn: at_turn == 0;
            invariant NoChildKilled: at_child == 0;
            invariant TheRestartIsRelaunched: decided <= 1;
            invariant NeverStrandedBehind: stranded == 0;
            invariant MovesOnlyUp: moved == 0 || landed == 1;
        }
    }
}

/// AGY'S LIVE UPGRADE.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_agy_quiet_upgrade_model() -> Model {
    aterm_spec::ty_model! {
        HarnessAgyQuietUpgrade {
            const Buggy = 0;
            var landed = 0;
            var turn = 0;
            var task = 0;
            var daemon = 0;
            var subagent = 0;
            var wake = 0;
            var shown = 0;
            var panel = 1;
            var draft = 0;
            var held = 0;
            var unread = 0;
            var shared = 0;
            var looked = 0;
            var moved = 0;
            var claimed = 0;
            var exited = 0;
            var decided = 0;
            var stranded = 0;
            var at_turn = 0;
            var at_task = 0;
            var at_daemon = 0;
            var at_subagent = 0;
            var at_wake = 0;
            var at_shared = 0;
            var slipped = 0;

            action LandNewer when (moved == 0 && landed == 0) {
                landed = 1;
            }
            action LandOlder when (moved == 0 && landed == 0) {
                landed = 2;
            }
            action TurnStarts when (moved == 0 && turn == 0) {
                turn = 1;
                shown = 1;
            }
            action TurnEnds when (moved == 0 && turn == 1) {
                turn = 0;
            }
            action ScreenBusy when (moved == 0 && shown == 0) {
                shown = 1;
            }
            action ScreenIdle when (moved == 0 && shown == 1) {
                shown = 0;
            }
            action TaskStarts when (moved == 0 && turn == 1 && task == 0) {
                task = 1;
            }
            action TaskEnds when (moved == 0 && task == 1) {
                task = 0;
                wake = 1;
            }
            action DaemonStarts when (moved == 0 && turn == 1 && daemon == 0) {
                daemon = 1;
            }
            action DaemonEnds when (moved == 0 && daemon == 1) {
                daemon = 0;
                wake = 1;
            }
            action SubagentStarts when (moved == 0 && turn == 1 && subagent == 0) {
                subagent = 1;
            }
            action SubagentIdle when (moved == 0 && subagent == 1) {
                subagent = 0;
                wake = 1;
            }
            action WakeTaken when (moved == 0 && wake == 1 && turn == 0) {
                wake = 0;
                turn = 1;
                shown = 1;
            }
            action PanelHidden when (moved == 0 && panel == 1) {
                panel = 0;
            }
            action PanelShown when (moved == 0 && panel == 0) {
                panel = 1;
            }
            action DraftTyped when (moved == 0 && draft == 0) {
                draft = 1;
            }
            action DraftCleared when (moved == 0 && draft == 1) {
                draft = 0;
            }
            action Hold when (moved == 0 && held == 0) {
                held = 1;
            }
            action Release when (moved == 0 && held == 1) {
                held = 0;
            }
            action LogUnread when (moved == 0 && unread == 0) {
                unread = 1;
            }
            action LogRead when (moved == 0 && unread == 1) {
                unread = 0;
            }
            action SharedOpens when (moved == 0 && shared == 0) {
                shared = 1;
            }
            action SharedCloses when (moved == 0 && shared == 1) {
                shared = 0;
            }
            action Look when (
                moved == 0 && looked == 0 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                looked = 1;
            }
            action ClaimWritten when (moved == 0 && looked == 1 && claimed == 0) {
                claimed = 1;
            }
            action ClaimUnwritten when (moved == 0 && looked == 1 && claimed == 0) {
                looked = 0;
            }
            action Restart when (
                moved == 0 && looked == 1 && claimed == 1 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                moved = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action TakeBack when (
                moved == 0 && looked == 1 && claimed == 1 &&
                (turn == 1 || task == 1 || daemon == 1 || subagent == 1 || wake == 1 ||
                 shown == 1 || draft == 1 || held == 1 || unread == 1 || shared == 1)
            ) {
                looked = 0;
                claimed = 0;
            }
            action Exit when (moved == 1 && exited == 0) {
                exited = 1;
            }
            action ReadExit when (exited == 1 && decided == 0) {
                decided = if claimed == 1 { 1 } else { 2 };
            }
            action RestartWithOpenWork when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && daemon == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0 &&
                (task == 1 || subagent == 1)
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action RestartWithDaemonTask when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 1 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action RestartMidTurn when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 1 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action RestartWithUnreadWake when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 1 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action RestartWhileShared when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 1
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action RestartUnclaimed when (
                Buggy == 1 && slipped == 0 && moved == 0 && looked == 1 && claimed == 0 &&
                landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                slipped = 1;
                moved = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action RestartOnStaleLook when (
                Buggy == 1 && slipped == 0 && moved == 0 && looked == 1 && claimed == 1 &&
                landed == 1 &&
                (turn == 1 || task == 1 || daemon == 1 || subagent == 1 || wake == 1 ||
                 shown == 1 || draft == 1 || held == 1 || unread == 1 || shared == 1)
            ) {
                slipped = 1;
                moved = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }
            action BuildReadAsOlder when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                slipped = 1;
                stranded = 1;
            }
            action BuildReadAsNewer when (
                Buggy == 1 && slipped == 0 && moved == 0 && landed == 2 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                slipped = 1;
                moved = 1;
                claimed = 1;
                at_turn = turn;
                at_task = task;
                at_daemon = daemon;
                at_subagent = subagent;
                at_wake = wake;
                at_shared = shared;
            }

            invariant NoRunningWorkInterrupted: at_task == 0 && at_subagent == 0;
            invariant NoDaemonKilled: at_daemon == 0;
            invariant NeverMidTurn: at_turn == 0;
            invariant NoWakeLost: at_wake == 0;
            invariant NeverStrandedDown: at_shared == 0;
            invariant TheRestartIsRelaunched: decided <= 1;
            invariant NeverStrandedBehind: stranded == 0;
            invariant MovesOnlyUp: moved == 0 || landed == 1;
        }
    }
}

/// AN AGY EXIT.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_agy_exit_witness_model() -> Model {
    aterm_spec::ty_model! {
        HarnessAgyExitWitness {
            const Buggy = 0;
            var exit = 0;
            var witness = 1;
            var word = 0;
            var later = 0;
            var swept = 0;
            var filed = 0;
            var seen = 0;
            var kept = 0;
            var decided = 0;

            action Crash when (exit == 0) {
                exit = 1;
            }
            action Graceful when (exit == 0) {
                exit = 2;
            }
            action Stop when (exit == 0) {
                exit = 3;
            }
            action Refuse when (exit == 0) {
                exit = 4;
            }
            action Panic when (exit == 0) {
                exit = 5;
            }
            action Unwitnessed when (exit == 0 && witness == 1) {
                witness = 0;
            }
            action ShellWord when (exit > 0 && word == 0 && decided == 0) {
                word = 1;
            }
            action Later when (word == 1 && later == 0 && decided == 0) {
                later = 1;
            }
            action Sweep when ((exit == 1 || exit == 5) && swept == 0 && decided == 0) {
                swept = 1;
            }
            action Look when (exit > 0 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                filed = if ((exit == 1 || exit == 5) && swept == 0) { 1 } else { 0 };
                kept = if exit == 3 {
                    3
                } else {
                    if exit == 2 {
                        2
                    } else {
                        if ((exit == 1 || exit == 5) && swept == 0) {
                            1
                        } else {
                            if (exit == 4 || exit == 5) { 4 } else { 3 }
                        }
                    }
                };
            }
            action LookGone when (exit > 0 && seen == 0 && (exit == 3 || word == 0 || later == 1)) {
                seen = 1;
                kept = 3;
            }
            action LookAsClaude when (Buggy == 1 && exit > 0 && seen == 0) {
                seen = 1;
                filed = if ((exit == 1 || exit == 5) && swept == 0 && word == 1 && later == 0) { 1 } else { 0 };
                kept = 1;
            }
            action RefusalAsCrash when (Buggy == 1 && exit == 4 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = 1;
            }
            action CrashFileIgnored when (Buggy == 1 && (exit == 1 || exit == 5) && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                filed = if swept == 0 { 1 } else { 0 };
                kept = if exit == 1 { 1 } else { 4 };
            }
            action Decide when (seen == 1 && decided == 0) {
                decided = if ((exit <= 2 || exit > 3)
                    && witness == 1
                    && (kept == 1
                        || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5) && swept == 0)))
                {
                    1
                } else {
                    2
                };
            }
            action StopAsCrash when (Buggy == 1 && seen == 1 && decided == 0 && exit == 3 && word == 1 && later == 0) {
                decided = if witness == 1 { 1 } else { 2 };
            }
            action DecideFresh when (Buggy == 1 && seen == 1 && decided == 0 && witness == 0) {
                decided = if ((exit <= 2 || exit > 3)
                    && (kept == 1
                        || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5) && swept == 0)))
                {
                    1
                } else {
                    2
                };
            }

            invariant TheirsIsLeft: exit <= 1 || exit > 2 || decided == 0 || decided == 2;
            invariant StoppedIsLeft: exit <= 2 || exit > 3 || decided == 0 || decided == 2;
            invariant ARefusalIsNeverRetried: exit <= 3 || exit > 4 || decided == 0 || decided == 2;
            invariant ACrashReadAtItsExitIsRelaunched: filed == 0 || witness == 0 || decided <= 1;
            invariant NothingRelaunchedOnAGuess: decided == 0
                || decided == 2
                || filed == 1
                || ((exit == 1 || exit == 5) && swept == 0 && word == 1 && later == 0);
            invariant NoWitnessNoRelaunch: witness == 1 || decided == 0 || decided == 2;
        }
    }
}

/// The upgrade's looks at one session.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_look_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeLook {
            const Buggy = 0;
            var due = 1;
            var readable = 0;
            var ready = 0;
            var looks = 1;
            var done = 0;
            var owed = 1;
            var noted = 0;
            var aged = 0;

            action Look when (looks == 1 && done == 0 && due == 1 && readable == 1) {
                ready = 1;
                done = ready;
                looks = if (ready == 1) { 0 } else { 1 };
                owed = 0;
                noted = 1;
                aged = if (noted == 1) { aged } else { owed };
            }
            action NotDue when (looks == 1 && done == 0 && due == 0 && readable == 1) {
                looks = 0;
                owed = 0;
            }
            action Unread when (looks == 1 && done == 0 && readable == 0) {
                looks = if (Buggy == 1) { 0 } else { 1 };
            }
            action Note when (owed == 1 && readable == 1) {
                owed = 0;
                noted = if (due == 1) { 1 } else { noted };
                aged = if (due == 1) { 1 } else { aged };
            }
            action NoteUnread when (owed == 1 && readable == 0) {
                owed = if (Buggy == 1) { 0 } else { 1 };
            }
            action Lose when (readable == 1) {
                readable = 0;
            }
            action Regain when (readable == 0) {
                readable = 1;
            }
            action Settle when (due == 1 && done == 0) {
                due = 0;
            }

            invariant NeverDropped: done == 1 || due == 0 || looks == 1;
            invariant BehindFromTheAttach: noted == 0 || aged == 1;
        }
    }
}

/// An age belongs to one process.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_age_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeAge {
            const Buggy = 0;
            var proc = 0;
            var rec = 1;
            var behind = 1;
            var aged = 0;
            var alarm = 0;

            action Time when (aged == 0 && rec > 0) {
                aged = 1;
            }
            action HandRestart when (proc == 0) {
                proc = 1;
                behind = 0;
                alarm = 0;
            }
            action Visit when (behind == 0 && rec > 0 && Buggy == 0) {
                rec = 0;
                aged = 0;
                alarm = 0;
            }
            action NewBuild when (behind == 0) {
                behind = 1;
                rec = if (rec > 0 && (Buggy == 1 || rec == proc + 1)) { rec } else { proc + 1 };
                aged = if (rec > 0 && (Buggy == 1 || rec == proc + 1)) { aged } else { 0 };
            }
            action Judge when (behind == 1 && aged == 1 && alarm == 0) {
                alarm = 1;
            }

            invariant AgeIsThisProcess: alarm == 0 || rec == proc + 1;
        }
    }
}

/// The explicit restart.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restart_stamp_model() -> Model {
    aterm_spec::ty_model! {
        HarnessRestartStamp {
            const Buggy = 0;
            var alive = 1;
            var stamped = 0;
            var inflight = 0;
            var fresh = 0;
            var termed = 0;
            var esc = 0;
            var committed = 0;
            var ended = 0;
            var at_end = 0;
            var read = 0;
            var hand = 0;
            var held_kill = 0;
            var into_claim = 0;
            var selfclaim = 0;
            var by = 0;
            var copy = 0;
            var lost = 0;
            var alab = 0;
            var lane = 0;

            action IsAlab when (alab == 0 && stamped == 0 && ended == 0) {
                alab = 1;
            }
            action Stamp when (alive == 1 && inflight == 0 && committed == 0 && hand == 0
                && (alab == 0 || copy == 1)) {
                inflight = 1;
                fresh = if (stamped == 1) { 0 } else { 1 };
                stamped = 1;
            }
            action Term when (inflight == 1 && alive == 1) {
                inflight = 0;
                fresh = 0;
                termed = 1;
                esc = 1;
            }
            action SendsNothing when (inflight == 1) {
                inflight = 0;
                fresh = 0;
                stamped = if (fresh == 1) { 0 } else { stamped };
            }
            action Hears when (alive == 1 && termed == 1) {
                alive = 0;
                ended = 1;
                at_end = stamped;
            }
            action Escalate when (esc == 1 && alive == 1 && hand == 0) {
                alive = 0;
                ended = 1;
                at_end = stamped;
                esc = 0;
                held_kill = hand;
            }
            action EscalationSeesExit when (esc == 1 && alive == 0) {
                esc = 0;
            }
            action EndsOtherwise when (alive == 1) {
                alive = 0;
                ended = 2;
                at_end = stamped;
            }
            action HandOn when (hand == 0) {
                hand = 1;
            }
            action ClaimOn when (hand <= 1 && selfclaim == 0) {
                hand = 2;
            }
            action SelfClaimOn when (alive == 1 && hand <= 1 && selfclaim == 0) {
                selfclaim = 1;
            }
            action SelfClaimOff when (selfclaim == 1) {
                selfclaim = 0;
                copy = if (alive == 1) { 0 } else { copy };
            }
            action Copied when (selfclaim == 1 && alive == 1) {
                copy = 1;
            }
            action HandOff when (hand > 0) {
                hand = 0;
            }
            action Commit when (committed == 0) {
                committed = 1;
                esc = 0;
                inflight = 0;
                fresh = 0;
                copy = if (alive == 1) { 0 } else { copy };
                selfclaim = if (alive == 1) { 0 } else { selfclaim };
            }
            action CarrierFinishes when (
                committed == 1 && stamped == 1 && alive == 1 && esc == 0 && hand == 0
            ) {
                alive = 0;
                ended = 1;
                at_end = 1;
                held_kill = hand;
            }
            action Read when (alive == 0 && read == 0 && hand == 0 && copy == 0 && alab == 0) {
                read = if (stamped == 1) { 1 } else { 2 };
                stamped = 0;
                into_claim = if (hand == 2) { 1 } else { 0 };
            }
            action Unclaimed when (alive == 0 && read == 0 && copy == 0 && alab == 1) {
                read = if (stamped == 1) { 4 } else { 2 };
            }
            action ReadByClaim when (alive == 0 && read == 0 && hand == 2) {
                read = 3;
                by = hand;
            }
            action CopyRelaunches when (alive == 0 && read == 0 && hand == 0 && copy == 1) {
                read = if (stamped == 1) { 1 } else { 2 };
                stamped = 0;
                copy = 0;
            }
            action CopyLeaves when (alive == 0 && read == 0 && hand == 0 && copy == 1) {
                read = if (stamped == 1) { 4 } else { 2 };
                copy = 0;
            }
            action SignalBeforeStamp when (
                Buggy == 1 && alive == 1 && stamped == 0 && termed == 0 && committed == 0
            ) {
                termed = 1;
                esc = 1;
            }
            action CommitForgetsStamp when (Buggy == 1 && committed == 0) {
                committed = 1;
                esc = 0;
                inflight = 0;
                fresh = 0;
                stamped = 0;
            }
            action TermOnly when (Buggy == 1 && esc == 1 && alive == 1) {
                esc = 0;
            }
            action StampAfterEnd when (Buggy == 1 && alive == 0 && stamped == 0 && read == 0) {
                stamped = 1;
            }
            action TakeBackAlways when (Buggy == 1 && inflight == 1) {
                inflight = 0;
                fresh = 0;
                stamped = 0;
            }
            action KillUnderHand when (Buggy == 1 && esc == 1 && alive == 1 && hand == 1) {
                alive = 0;
                ended = 1;
                at_end = stamped;
                esc = 0;
                held_kill = hand;
            }
            action DecidedWhileRunning when (
                Buggy == 1 && alive == 1 && stamped == 1 && read == 0
            ) {
                read = 2;
            }
            action WatchPerAct when (Buggy == 1 && inflight == 1 && alive == 1 && esc == 1) {
                inflight = 0;
                fresh = 0;
                termed = 1;
                esc = 2;
            }
            action LeftToTheHolder when (Buggy == 1 && alive == 0 && read == 0 && hand == 1) {
                read = 2;
            }
            action CarriedIntoAClaim when (
                Buggy == 1 && alive == 0 && read == 0 && hand == 2 && stamped == 1
            ) {
                read = 1;
                stamped = 0;
                into_claim = 1;
            }

            action DeadSelfClaimTakesTheExit when (
                Buggy == 1 && alive == 0 && read == 0 && hand == 0 && selfclaim == 1
            ) {
                read = 3;
            }
            action CommitDropsTheCopy when (
                Buggy == 1 && committed == 0 && alive == 0 && copy == 1
            ) {
                committed = 1;
                esc = 0;
                inflight = 0;
                fresh = 0;
                copy = 0;
                lost = 1;
            }
            action ReleaseHandsToLane when (
                Buggy == 1 && alive == 0 && read == 0 && hand == 0 && copy == 0 && alab == 1
            ) {
                read = if (stamped == 1) { 1 } else { 2 };
                stamped = 0;
                lane = 1;
            }

            invariant TheActsEndIsRelaunched: read <= 1 || (read == 3 && by == 2) || read == 4
                || ended == 0 || ended == 2;
            invariant OnlyAStampedEndIsTheActs: read == 0 || read == 2 || read == 3 || read == 4
                || at_end == 1;
            invariant TheActEndsItsProgram: alive == 0 || termed == 0 || esc > 0
                || committed == 1;
            invariant TheKillWaitsOutAnotherHand: held_kill == 0;
            invariant OneWatchPerProcess: esc <= 1;
            invariant NothingIsTypedIntoAClaim: into_claim == 0;
            invariant TheCopysDecisionRidesTheCommit: lost == 0;
            invariant NoCodexEndIsTheLanes: lane == 0;
        }
    }
}

/// The restart ends its program ([`harness_restart_stamp_model`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_restart_stamp_model)]
pub fn harness_restart_stamp_liveness() -> aterm_spec::derive::Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    aterm_spec::derive::Liveness {
        name: "TheRestartEndsItsProgram",
        goal: or_(eq(var("stamped"), int(0)), eq(var("alive"), int(0))),
        weak: vec!["Term", "HandOff"],
        strong: vec!["Escalate", "CarrierFinishes"],
        mutants: vec!["TermOnly"],
    }
}
