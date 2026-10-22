// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-GUI supervisor host's per-session machines: the restore queue and its carry across a
//! seamless update, the relaunch a failed Commit owes, the worker lifecycle (one supervisor
//! per session, a failing one restarted for ever), the supervisor claim across an update, which
//! exits the host hands a worker, the relaunch on exit and how an exit is read, the upgrade's
//! looks and ages, and the explicit restart.

use aterm_spec::derive::Model;

/// Two restored tabs in the host's one retry queue. A's first step waits; B still gets its
/// first step before A can retry, even when A's due time passes during its own call. `Buggy=1`
/// is a nested retry loop that runs A's second step before B's first. Tier-1 drives the real
/// host's restored worker with these outcomes and validates each observed action.
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

/// Harness work in flight at a seamless update's Commit: one restored agent `A` a cold restore
/// queued ([`harness_restored_first_attempt_model`]'s queue) and one restart `R` the outgoing
/// instance left in flight in a tab whose agent it had already ended (a shell tab no worker
/// looks at).
///
/// `queued`: `A` is owed by the outgoing instance's queue; `acting`: its step runs (still
/// queued); `typed`: a step typed its line and waits for its agent (`wait:resume`), so its
/// relaunch is in flight on the conversation's record and every later step, here or the
/// successor's, carries that on without typing again; `ran`: an agent ran in `A`'s tab since it
/// was queued (a ghost); `overtaken`: the host knows it, of an `A` that typed nothing (kept once
/// seen); `paused`: a handoff parked the readers; `captured`: what the park's pending layout
/// carried for `A`; `capture`: how a layout reads the queue (0 live; the mutants' 1 dropping it
/// and 2 the copy the park froze); `committed`: the successor took over (the outgoing instance
/// and its running step end there); `succ`: the successor's queue owes `A`; `resolved`: `A` was
/// relaunched or said; `late`: a step started while parked; `over`: a relaunch of an `A` that
/// typed nothing stepped after an agent ran in its tab; `restart`: `R`'s record is in flight;
/// `swept`: the successor's Commit owes `R` a carry-on step.
///
/// * `StepStart`, `StepRetries`, `StepTypes`, `StepResolves`: the outgoing worker's step and its
///   ends (a word not yet possible, typed nothing, keeps `A` queued; one that typed and waits
///   keeps it queued, typed). No step starts while parked or once an agent is known to have run
///   in the tab of an `A` that typed nothing; the step in flight ends while parked too.
/// * `AgentRuns`: anyone starts an agent in `A`'s tab, at any time, and the roster names it. It
///   overtakes an `A` that typed nothing, not one that typed (`RestoredAttempt::typed`).
///   `Overtaken`: the queue takes `A` off without a step (never while parked: its worker waits
///   on the park).
/// * `Park` captures the pending layout from the queue as it is (`A` while queued and not
///   overtaken); `Rollback` resumes the queue.
/// * `Commit` re-captures the layout the same way and goes ahead only where the two captures
///   agree (a step that landed or an agent that ran in between refuses it, as activity),
///   handing what they carry to the successor's queue and owing every restart record in flight
///   a carry-on.
/// * `SuccRelaunches`, `SweepCarriesOn`: the successor's steps, each the same idempotent step
///   the outgoing instance would have taken, so a relaunch already typed is never typed twice.
/// * `RestartFinishes`: the outgoing instance finished `R` before the Commit.
///
/// A typed `A` crosses a handoff untyped: the successor queues what the leaf carried as a fresh
/// attempt, so the agent the outgoing line started overtakes it there and the queue takes it
/// off with no row. What it typed is the conversation's record in flight, which the successor's
/// Commit sweeps on every adopted tab (`R`'s carry-on, the same step), so `SuccRelaunches` here
/// is that one step, whichever takes it.
///
/// * `NoSilentLoss`: past the Commit `A` is resolved, the successor owes it, or an agent ran in
///   its tab;
/// * `NoStepWhileParked`: the worker takes no new step while the terminal is parked;
/// * `NoRelaunchOverAgent`: no relaunch of `A` is stepped, here or by the successor, once an
///   agent ran in its tab having typed nothing;
/// * `NoStrandedRestart`: past the Commit a restart record in flight is owed a carry-on.
///
/// `Buggy=1`, each mutant caught alone: `ParkDropsQueue` (a handoff leaf that names no queued
/// agent; `NoSilentLoss`), `StepStartWhileParked` (a worker the park does not stop),
/// `CommitWithoutSweep` (a successor that resumes its host for agent tabs only;
/// `NoStrandedRestart`), `ParkFreezes` (both captures read the queue frozen at the park and
/// always agree, so the successor types its relaunch beside an agent a person started there;
/// `NoRelaunchOverAgent`), `StepStartOvertaken` (a queue that never asks the tab before a step)
/// and `AgentStopsForgotten` (asking whether an agent runs only at the instant of a capture or a
/// step, which a ctrl-z or its exit clears). The bounded hold that keeps an automatic update
/// from parking while `A` is queued is the ladder's (`native_update_apply_ladder_model`'s
/// `warmup`); this machine is what an explicit update, and one past the hold's bound, relies
/// on. Tier-1: `aterm-gui`'s `the_real_restored_carry_conforms_to_its_model` drives the real
/// host's queue through each of these and projects every observed state onto the model's.
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

/// A crashed agent's relaunch through a park's Commit that fails. `aterm-gui`'s `harness_host`
/// for one session: its agent, its worker, the mark a Commit leaves on the session
/// (`Kept::commit`) and the mark of an exit no worker was handed (`Kept::unwatched`), through
/// one Commit that fails and rolls back, then the next park, or a quit.
///
/// Variables:
///
/// * `agent`: 1 runs in its tab, 0 crashed. `worker`: 0 none, 1 supervising (no exit handed), 2
///   handed the exit (`left`) and not read it, 3 read it, the relaunch owed on its back-off.
///   `stop`: that worker's stop is set (the Commit's or a hand-over's). `handover`: a changed
///   policy handed it over (`Worker::hand_over`).
/// * `commit`: 0 `No`, 1 `Stopped`, 2 `Stranded`, 3 `TakenUp`. `unwatched`: a worker the Commit
///   stopped, or one handed over, ended handed nothing, or the host read an agent back in the
///   tab of a worker handed its exit.
/// * `phase`: 0 before the Commit, 1 the Commit holds the host (suspended: no pass hands an
///   exit), 2 the Commit failed (the host supervises again), 3 rolled back
///   (`relaunch_stranded`), 4 the next park, 5 its Commit went ahead.
/// * `done`: a worker relaunched the agent since its last crash; `queued`: its relaunch is on
///   the restored queue; `stale`: an overtaken cold-restore relaunch of the same tab is still
///   queued; `captured`: what the next park's layout carries for the tab; `succ`: that Commit
///   handed the successor the relaunch; `dropped`: it went ahead with a relaunch queued for the
///   tab that its layout did not carry.
/// * `blind`: the host's mark holds an exit the worker it is handed to cannot decide (only
///   `SettleGone` sets it). `closed`: the host shut down and its restored queue is closed; the
///   quit layout carries what the queue and the sessions still owe. `down`: the host thread
///   panicked and waits out its restart's back-off (no pass hands an exit or a mark, or starts
///   a worker).
/// * `seen`: the last exit was read as it was seen, by the worker a pass handed it at once or by
///   the exit watch (`Kept::exit_watch`). `swept`: a Claude Code started since and removed the
///   crashed agent's session record, as every Claude Code that starts removes dead agents'
///   records. `landed`: the agent a relaunch landed, not yet named by a pass, is watched by the
///   exit watch, which hands its exit itself (`ExitWatched::hands`). `qlanded`: the restored
///   queue's step landed an agent no read has reached; its attempt stays queued, the exit's
///   holder, until a read reaches it (`QueueReadsLanded`), a pass names it (`Start`), or it
///   crashes unread and is stepped again.
///
/// Actions:
///
/// `Buggy=1` is what shipped, each mutant caught alone: `DecideByPark`
/// (round ten read a Commit's stop as `Switches::parked`, which the
/// rollback lifts — a worker deciding after it settled), `StopEndsUnread`
/// (round eleven read the exit from `left`, which the Commit's stop clears
/// — a crash handed before the stop was never read), `EndUnhandedForgotten`
/// (round twelve: a worker the Commit stopped before any exit was handed
/// ended, and no pass hands an exit to a stopped worker) — each
/// `NoOwedRelaunchLost` — `EndHandedOverForgotten` (round thirteen marked
/// only a worker the Commit had marked: one a policy change handed over just
/// before the Commit's stop was not, and the crash during the hold was read
/// by nobody — `NoOwedRelaunchLost`), `GoneReadAsStop` (until the joint audit
/// of 2026-10-01 one flag, `left`, said both "the agent is back" and "a
/// stop": the host marked nothing as it read the agent back, and a worker
/// that then read it gone again took the cleared flag for a stop for good —
/// or its relaunch landed — and ended, settled, with nothing holding the
/// exit — `NoOwedRelaunchLost`; the model missed it while `Back` settled the
/// worker at once and no crash came while a worker still held the agent),
/// `SettleGone` (until round two of that audit a relaunch that landed ended
/// its worker whatever became of the agent it started: one the host read
/// back, or one a Commit's stop came for, gone before the worker read it,
/// left its exit to a worker the host handed it, which could not decide it —
/// `nothing-recorded`, or the record swept and read as a graceful exit —
/// `NoOwedRelaunchLost`; the model encoded that path as a landing's `Settle`
/// while its Tier-1's seams answered a follow of another process `Agent`
/// and read a snapshot at a bare shell, as no live seam does),
/// `DecideClosedDrops` (until round sixteen a worker told `TakenUp` that
/// decided after a quit closed the queue had its relaunch refused and owed
/// it no more: nothing held it for the quit layout — `NoOwedRelaunchLost`),
/// `PanicStops` (until round seventeen a panic of the host's loop dropped its
/// workers, which stopped each with no hand-over and no Commit: a worker
/// supervising ended leaving no exit to the host, and one handed its exit
/// settled what it owed as a stop for good — `NoOwedRelaunchLost`),
/// `CrashUnseen` (until round seventeen an exit left to the host while a
/// Commit held it was read only by the worker the host handed it once it
/// supervised again: a crash's record a Claude Code started during the hold
/// had removed read as a graceful exit — `NoOwedRelaunchLost`),
/// `LandedLost` (until the review of round seventeen's first cut the agent
/// a relaunch landed was left to the roster with nothing watching it: its
/// crash before a pass named it — the moment before the next pass, but a
/// panic's whole back-off while the host was down, through which the worker
/// carries on — was read by nobody — `NoOwedRelaunchLost`),
/// `QueueLandedLost` (until round eighteen the restored queue's landing
/// left its agent to the roster with nothing watching it, the queue's own
/// thread running on through a host panic's back-off: its crash before a
/// pass named it was read by nobody — `NoOwedRelaunchLost`),
/// `UnreadLandedLost` (until round eighteen a landing whose agent no read
/// reached ended its worker with nothing watching it — no process to watch,
/// no mark: its crash before a pass named it was read by nobody —
/// `NoOwedRelaunchLost`), `RestartPanicStops` (until round eighteen a panic
/// in the host's restart, past the run's catch, unwound the host thread
/// and dropped its workers, each stopped with no hand-over and no Commit —
/// `NoOwedRelaunchLost`), `QueueUnreadLost` (round eighteen's first cut:
/// the queue's landing no read reached ended its attempt, and the thread
/// that read it again gave up once the agent no longer held its tab — its
/// crash before any read was held by nobody — `NoOwedRelaunchLost`)
/// and `ParkFirstFound` (round twelve's
/// captures read the first relaunch queued for the tab, the overtaken one —
/// ONE reader, so the mutant sets `firstfound` and the Commit's re-capture,
/// the same code, reads that way too: both carried nothing, agreed, and the
/// Commit went ahead — `NoQueuedRelaunchDropped`). One mutant, not a park
/// mutant and a Commit mutant: either capture alone reading first-found
/// disagrees with the other and refuses the Commit, so neither alone is a
/// defect a checker could catch (`ty --strict-vacuity`). Tier-1 (`aterm-gui`'s
/// `harness_host` tests `the_real_commit_owed_relaunch_conforms_to_its_model`,
/// `a_crash_after_a_hand_over_conforms_to_the_commit_owed_model`,
/// `an_agent_back_and_gone_again_conforms_to_the_commit_owed_model`, and the exit watch's
/// `an_exit_nobody_is_handed_conforms_to_the_commit_owed_model` and
/// `the_landings_of_round_eighteen_conform_to_the_commit_owed_model` drive the real host through
/// these paths and project each observed state onto the model's.
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

/// `aterm-gui`'s `harness_host` runs one worker per agent session. For one session: `wanted` is
/// the session's published program being a supervisable agent (or, for a pass that cannot name
/// it, its agent still holding the tab, [`harness_leave_model`]) with the policy active; `on`
/// that agent's own switch (`[harness.<id>] enabled`); `cur` a worker under the current policy;
/// `old` workers asked to stop whose wait has not ended; `faults` the session's failed runs in
/// the restart window, across its workers, kept at most `Budget + 1`; `faulted` the session
/// badged while its worker waits to restart a supervisor failing past the budget (`cur` stays);
/// `held` another supervisor's claim refused ours.
///
/// * `Arrive`/`Leave`: the program becomes or stops being the agent. Leaving asks the worker to
///   stop and forgets `held`, but keeps `faults`: a program flap gives a crash-looping engine
///   no fresh budget. The badge goes with its worker at `Exit`.
/// * `SwitchOff`/`SwitchOn`: off stops the worker exactly as `Leave` does while the session
///   stays the agent's (a stop, never the agent leaving: [`harness_leave_model`] hands its
///   worker no exit); on lets `Start` run again.
/// * `Start`: a worker for a wanted session whose agent is on, with none of its own, not held,
///   and no old worker still running (the host reaps a stopped worker before starting the
///   next). It inherits `faults`.
/// * `Exit`: an old worker's wait ends and it is reaped, its badge cleared.
/// * `Fail`: the current worker's run failed or panicked and restarts in place, always: within
///   `Budget` on the short pauses, past it badged while it waits for the longer one.
/// * `Restart`: `faulted` is withdrawn when a run outlives its failure
///   (`HostNote::SupervisorSteady`), not when the next run merely starts. `Exit` and `Hold`
///   withdraw it too.
/// * `Age`: the oldest failure leaves the hour's window.
/// * `Hold`: the claim was refused and the worker ends; `Release`: a claim was released or
///   lapsed somewhere, so a held session may be tried again.
/// * `Reload`: the `[harness]` policy changed: the worker is asked to stop (its successor starts
///   under the new policy) and `faults` is forgiven.
///
/// Autopilot Off (`docs/DESIGN-harness-autopilot-2026-10-15.md` §2.2): `off` is the session's
/// effective Autopilot being Off (its own word, else the table's default), an open tab the
/// harness keeps its hands off. It is not the agent's switch `on`: the switch is
/// `[harness.<id>]`'s and covers every session of that agent, the word is one session's. Both
/// stop the worker and neither lets it go.
///
/// * `TurnOff`: the word goes Off and the host stops the worker (`old` until `Exit`), never
///   lets it go: a let-go hands the worker its agent's exit and begins the relaunch decision.
/// * `TurnOn`: the word goes back to Cruise or Land, and `Start` may run again.
/// * `Crash`: the agent exits on its own while its tab stays open. Under Cruise or Land the
///   running worker is handed the exit; Off has no running worker, so nothing is handed or
///   relaunched. `letgo` records an exit handed to a worker while the session is Off.
/// * `Start` waits for `off == 0` too: no worker starts for an Off session, whether its agent
///   is named again or was never stopped.
///
/// A VENDOR CODEX (the Codex screen lane's retirement, 2026-10-16; `vendor`):
/// `ArriveVendor` is the roster naming a Codex session with no proven
/// self-claim, no copy of its record and no ALab build in front
/// (`harness_host::FollowStamp::vendor`). aterm supervises no Codex: such a
/// session is shown and never admitted, so `Start` waits for `vendor == 0`
/// (`NoWorkerForAVendorCodex`); codex-alab arrives as any agent does, its
/// worker watching only. `Leave` and `Crash` end it with the session.
///
/// `Buggy=1` is a host without its guards, each one a defect with its own
/// witness: it starts the new worker without waiting for the old one to end
/// (two supervisors answering one session's boxes, the double press the claim
/// exists to prevent — `OneSupervisor`) and over another supervisor's claim
/// (`HeldIsOff`), it counts failures past the window's cap (`FaultBudget`),
/// and it GIVES UP on a supervisor failing past the budget — the worker
/// ended with the badge up, nobody supervising and nothing to restart it
/// (`NeverGivesUp`: a session whose failure the window was told of is
/// still supervised, or its worker is on its way out). It starts a worker
/// for a session whose agent is switched off (`Start` not reading the switch
/// — `OffMeansNoCurrent`), and one for a vendor Codex, admitted by the
/// identity alone as the Codex lane did (`VendorAdmitted`:
/// `NoWorkerForAVendorCodex`). For Autopilot Off it starts a worker for an Off
/// session (`OffStopsTheWorker`), and its `TurnOff` is a let-go — Off
/// implemented as `Worker::leave` — handing the running worker its agent's
/// exit, the relaunch decision begun (`OffLetsNothingGo`); a crash of the Off
/// session it started a worker for is handed too. Every defect is a `Buggy`
/// branch of a live action — `Start`'s one branch drops every one of its
/// guards, the wait, the claim, the switch, the word and the vendor gate alike — so no action
/// is dead at `Buggy=0` (the strict-vacuity audit). The shipped
/// guard on `Fail` is `cur == 1` alone — `faults` never passes `Budget + 1`
/// there — and the `faults <= Budget + 1` half only bounds the buggy host's
/// space. Tier-1 (`aterm-gui`'s `harness_host` conformance) drives the real
/// host through each action but `Restart` and `Age`, and checks its
/// observed state against the model's after the same actions. A stopped
/// worker's body lingers while the step holds the state its stop leaves
/// (about two host passes after the stop settles), so a host that starts
/// the next worker before reaping the old one within those passes (at a
/// reload, at a flap) is seen with two (`OneSupervisor`) — a start delayed
/// past the hold is not witnessed here; and one failure
/// is driven past the cap within the window, so a history kept past
/// `Budget + 1` is seen (`FaultBudget`). The switch
/// is read from the host's live per-agent gate, with a running worker
/// switched off and back on after failures: the old worker ends, a new one
/// starts, and the session's failures survive. The held-session off/on
/// trace is a model-only check here, not a runtime claim. Autopilot's word
/// is driven too: `letgo` is OBSERVED there (a worker's body read its
/// agent's exit handed with its stop), never only set by `Buggy`:
/// `the_lifecycle_projection_catches_a_real_let_go_of_an_off_session` lets
/// the real host's worker of an Off session go inside the pass that read it
/// Off, and the projection breaks `OffLetsNothingGo`. `ArriveVendor` is
/// driven through the real roster gate (`harness_host::admits`, a vendor
/// stamp): no worker starts across two passes, and the same Codex stamped as
/// codex-alab gets one; NEGATIVE CONTROL: the host given a vendor Codex with
/// no vendor fact — today's identity-only admission — starts a worker, and
/// that projection breaks `NoWorkerForAVendorCodex`
/// (`the_real_host_admits_no_worker_for_a_vendor_codex`).
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

/// One session's supervisor claim across a seamless update: an external supervisor that held
/// the session in the outgoing instance is never displaced by the incoming instance's own
/// in-GUI host.
///
/// Before the Commit (`phase` 0, the outgoing instance) the session may be held by an external
/// supervisor's `ttl=` lease (`ext` 1), by a claim bound to its connection (`ext` 2), or by the
/// outgoing instance's own host (`own`); the outgoing build may predate the carry (`older`) and
/// write no claim facts. `Park` draws the handoff record. `Commit` is the incoming instance's
/// adoption as the record allows: `server` is the claim it shows (0 none, 1 the external
/// holder's, 2 its own host's), only a carried lease seeded, and `grace` the ticks its host
/// holds the session off, unless the record vouched that only the outgoing host held it
/// (`SessionRecord::claim_grace`).
///
/// After the Commit time passes (`Tick`) while the grace runs or a live external holder owes
/// its renewal, which arrives within `Renew` ticks (`ExternalRenews`: a renewed lease, or a new
/// claim after its connection's ended; refused if the host took the session). A carried lease
/// may lapse first (`Lapse`: a short `ttl=`, or a renewal the outgoing instance took after the
/// park). `HostClaims` is the incoming host claiming a free session once its grace is over;
/// `took` records a claim made before a live external holder's renewal arrived.
///
/// * `NoTakeover`: the incoming host never takes a session a live external supervisor held
///   (`Grace` must outlast `Renew`: `harness_host::ADOPTED_CLAIM_GRACE` is two renewal steps
///   and a margin);
/// * `NoPhantomClaim`: the incoming instance never shows a claim no external supervisor made;
/// * `NoNeedlessGrace`: a session the record vouched for is supervised at the Commit.
///
/// `Buggy=1` is three dead Commits: `CommitUncarried` (no claim carried and no grace, so the
/// host claims at the Commit and the external renewal is refused; `NoTakeover`),
/// `CommitOwnLease` (carrying the outgoing host's own lease, parking the incoming host behind a
/// dead process's claim; `NoPhantomClaim`), `CommitGraceAll` (a grace on every session;
/// `NoNeedlessGrace`). Tier-1: aterm-gui's `session_store` test
/// `the_real_claim_carry_conforms_to_the_handoff_claim_model` drives the real record
/// projection, TOML wire, seed and grace decision through every pre-Commit configuration;
/// `harness_host`'s grace test drives the real host's gate.
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

/// The host hands a worker its agent's exit only for an exit. `aterm-gui`'s `harness_host`, for
/// one supervised session whose tab lives on, under `[harness] relaunch = false`. `alive`: the
/// fact the host reads that the agent holds its tab (`Acts::holds` answering `Some(true)`: the
/// group it was named in is still the tab's foreground, its leader exists, and nothing read for
/// it since could not host it), the hold rather than the agent itself (see below); `named`: the
/// roster names it (its published program, or its reader); `ours`: the worker's own upgrade
/// step ended it and the relaunch that step makes has not landed; `stray`: the tab has an
/// upgrade restart in flight that is not this agent's (another process's, a relaunch's own
/// record, or one too old to act on); `due`: the host owes itself a pass (its bell rang, or a
/// timed look is set); `handed`: the host handed the worker the exit (`Worker::leave`);
/// `decided`: 1 carried on (relaunched), 2 left and said (the owner's limit); `off`: the agent
/// was switched off (`[harness.<id>] enabled = false`) before the exit was handed.
///
/// * `Misread`/`Reread`: the roster stops or starts naming a live agent: a name the resolver
///   could not read replaces the good one (`set_program(None)`) while no reader identifies the
///   agent (the roster names it by its reader alone when it can, `agent_of`), or a
///   runtime-named agent (`node`) loses its reader. Either moves the published name or reader,
///   which rings the host.
/// * `Exit`: the agent ends on its own. Its foreground group moves, which rings the host only
///   while it was named (`SessionTimeline::note_foreground_group`). An exit during a misread
///   rings nothing here, the conservative case: live, a runtime-named agent's `node` goes with
///   its group, and a name-failed one's exit rings as the shell's name resolves
///   (`set_program`); only when that fails too does nothing ring, and the look `Visit` sets is
///   the backstop. Keep those rings even though the model is green without them.
/// * `Terminate`: the worker's upgrade step ended the agent (SIGTERM) and its relaunch waited;
///   the step's end rings the host (`note_upgrade_act`). The host takes nothing while the step
///   runs (`acting`), so the step is one action.
/// * `Stray`: a restart record for the tab that is not this agent's appears (no ring).
/// * `SwitchOff`: the changed policy rings the host; taken only before an exit is handed.
/// * `Visit`: a host pass, which reads the hold (`Acts::holds`) for a worker the roster does
///   not name: an agent that still holds its tab is no exit, so the worker is kept and a short
///   look set (nothing else would ring for an exit that follows); one that no longer holds it
///   is handed the exit. A switched-off agent's worker is stopped through
///   [`harness_worker_lifecycle_model`]'s `SwitchOff`: handed nothing and owed no look.
/// * `Decide`: the worker handles the exit (`on_agent_left`): the upgrade's own restart in
///   flight, asked about the leaving agent's pid (`Acts::restarted`, `relaunch::restarted`), is
///   carried whatever `[harness] relaunch` says; its own exit is left, and said.
///
/// `NoExitWhileAlive`: no worker is handed the exit of an agent that still holds its tab.
/// `NoExitWhileOff`: nor of an agent switched off (a stop is no exit). `NeverMissed`: an exit
/// is handed, a pass is still owed, or the agent is switched off. `TheUpgradesOwnIsCarried`: an
/// agent the upgrade ended is never left for `[harness] relaunch = false`.
/// `OnlyTheUpgradesOwnIsCarried`: nothing else is carried against it. The invariants restate
/// `Visit`'s and `Decide`'s own expressions, so they are non-vacuous only through the `Buggy`
/// hosts.
///
/// Not modelled: a hold by something that is not the agent (a zombie leader not yet reaped, a
/// recycled leader pid, a runtime that outlived its agent in the group, an `exec` no name was
/// read for), so `NoExitWhileAlive` is about the host's reading of the hold, not the agent. The
/// keep's ceiling (`HOLDS_KEEP_MAX`, 30 s) bounds such a keep, and past it the host hands even a
/// live agent's exit, which a model with no time does not show. A hold that cannot be read
/// (`None`: a ConPTY has no foreground group) keeps nothing; it is `alive = 0` here. The worker
/// restarted for a kept session, `still_wanted`, another group named in a kept agent's stead,
/// the ceiling and the hold itself are bound by `aterm-gui`'s `harness_host` tests
/// (`a_kept_agent_is_looked_at_again_and_nothing_else_is_kept`,
/// `another_group_named_in_a_kept_agents_stead_is_its_exit`,
/// `a_kept_agent_is_followed_at_full_follows_not_at_every_bell`,
/// `a_pass_that_keeps_an_agent_owes_the_host_a_look`, and on a real
/// terminal `the_real_hold_reads_the_tabs_foreground_and_its_leader` and
/// `the_hold_reads_the_groups_leader`).
/// A switch-off after the exit was handed — between `Visit` and `Decide` —
/// is outside the model too: `SwitchOff` is guarded by `handed == 0` and
/// `Decide` is one action here, so whether such a worker still relaunches
/// its agent is the host's to bind, and nothing binds it yet.
///
/// `Buggy=1` adds five hosts, each a dead action caught alone: a pass that hands the exit
/// whenever the roster does not name the agent (`VisitUnconfirmed`: `NoExitWhileAlive`); one
/// that keeps the worker but sets no look (`VisitNoLook`: `NeverMissed`); an exit decision that
/// reads `[harness] relaunch` alone, dropping the upgrade's own SIGTERM as the agent leaving
/// (`DecideLimited`: `TheUpgradesOwnIsCarried`); one that reads the restart record by tab
/// alone, relaunching a person's crashed agent against the owner's limit (`DecideByTab`:
/// `OnlyTheUpgradesOwnIsCarried`); and a host that drops a switched-off agent from its roster
/// and hands its worker the exit (`VisitOffAsLeave`: `NoExitWhileAlive`, or `NoExitWhileOff`
/// after an `Exit`).
///
/// Tier-1: `the_real_host_conforms_to_the_leave_model` drives the real host through every path
/// of the environment's actions but `SwitchOff` and checks after each that what it did (kept,
/// carried on or said) is the model's. Only `handed` and `decided` are observed there; `alive`,
/// `ours`, `stray` and `named` are what its driver did to a fake world, `due` is the model's,
/// and its negative controls fire `VisitUnconfirmed` and `DecideByTab` on model state alone.
/// `the_real_host_switch_off_conforms_to_the_leave_model` binds `SwitchOff` of a named or
/// misread live agent and its later exit: `off` reads the host's live per-agent gate, the
/// worker ends without an exit decision, and another admitted agent starting after the exit
/// witnesses that a host pass reached the start decisions. `VisitOffAsLeave` stays a
/// model-state negative control. `a_pass_that_keeps_an_agent_owes_the_host_a_look` binds the
/// look `Visit` keeps owed to the instant the real `host_pass` waits for, the one test that
/// catches `VisitNoLook`.
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

/// `aterm-agent`'s `harness::relaunch` as the host drives it, for ONE
/// session's agent: `running` the agent holds its tab; `asked` the tab is
/// LEFT — the last exit was a person's (a keystroke within `human_grace_s`
/// of it, or a person who came back during the back-off), a holder's (a
/// halt, a lease, a named driver's turn), or the launch's own end — and
/// nothing is typed into it; `pending` a relaunch is due; `badged` the
/// session's attention says the relaunch is limited, cannot be made or keeps
/// failing; `step` the back-off step the next attempt waits
/// (`Relaunches::pause`, `Steps` the last, ten minutes: a crash's on the
/// crash back-off, an exit someone asked for on its own steps from the
/// first, so a crash history never paces a person's restart); `misses` the
/// attempts in a row that did not relaunch, capped at `Badge`; `restarted`
/// the exit being handled was made by a RESTART OF THE HARNESS'S OWN still
/// owed its relaunch (S0 of the in-flight review, 2026-09-27); `crash` the
/// exit being handled is a crash, an exit nobody asked for; `streak` the
/// CRASH back-off (`Relaunches`' `streak`: the step a crash's first attempt
/// waits, which only a crash's attempts climb — `step` is past it by the
/// misses of an exit that is none); `base` the streak as that exit came (a
/// history variable: what an exit that is no crash leaves it at); `asks` the
/// session's standing word asks a person to resume a conversation (the tab's
/// mark naming its command: `Relaunches::ask`).
///
/// * `Restarted` — the agent left because the harness's own restart ended
///   it (the upgrade's SIGTERM, a Codex `/exit` it typed, the restart in
///   place's): whoever is at the tab, and whatever the owner's `[harness]
///   relaunch` says, the relaunch is due — the exit is that restart's, and
///   what is typed at the prompt still waits on a person or a holder (the
///   relaunch line's own look). No person returning, no limit and no "end
///   of the launch" leaves it: it lands, keeps being tried, or is said.
/// * `Crash` — the agent left with no person asking: a relaunch is due.
/// * `Asked` — the EXPLICIT RESTART ended it (`aterm ctl restart`, the
///   stall's remedy; its stamp names the process): a relaunch is due, whoever
///   typed just before, and it is no crash: its
///   attempts and its landing leave the crash back-off where it was. Nor is
///   `Restarted`.
/// * `Lapse` — the explicit restart's stamp lapsed before its program ended:
///   from here the end is the program's own, a crash's (`OnExit::Relaunch`
///   decided again over the asked exit).
/// * `PersonExit` — a person's `/exit`, ctrl-c twice, or a holder's exit:
///   the tab is theirs.
/// * `Limited` — nobody owns the exit (or the pending relaunch), but the
///   owner's `[harness] relaunch = false`, or an agent the relaunch is not
///   written for, takes the power away: nothing due, and said.
/// * `PersonRuns` — the person starts an agent there themselves: nothing
///   is due, and the relaunch's word goes — save one that asks for a
///   conversation (`asks`), which no agent but that conversation's answers
///   (the told command fallen into onboarding, or
///   another conversation, cleared it).
/// * `Ask` — the host names, on the standing word (the tab's mark), the
///   conversation it asks a person to resume.
/// * `failing` — the standing word is the relaunch's that keeps failing
///   ("aterm still tries", raised by the `Badge`th `Miss`): it ends with the
///   trying (the review of the r20 relaunch lane, 2026-10-16). Its own end
///   and a person or a holder back (`Ended`, `PersonReturns`) clear it, and
///   the conversation it asked for; a limit or a refusal (`Limited`,
///   `Cannot`) says its own word in its place, asking for nothing until the
///   host names it again (`Ask`). An agent run in the tab meanwhile is
///   `PersonReturns` then `PersonRuns`.
/// * `said` — the relaunch's failing was said BESIDE a standing word that
///   asks for a conversation (`asks`, which keeps the attention: the
///   `Badge`th `Miss` with no `failing` word of its own), its line of the
///   digest: it ends with the trying as `failing` does, that word, its mark
///   and its ask left standing (the review of the harness landing's eighth
///   merge).
/// * `AskedRuns` — the agent in the tab is read holding that conversation:
///   the word is answered and goes.
/// * `Land` — an attempt relaunched it: running again, the misses and the
///   badge forgotten, the back-off one step longer — a crash's (`crash`).
/// * `Miss` — an attempt that did not (the shell's prompt not back, the
///   relaunch not registered): tried again one step later, and said to a
///   person once `Badge` missed in a row — and still tried. (Another actor
///   on the upgrade lock is no miss: a stutter, the state unchanged.)
/// * `Ended` — an attempt found the exit was the launch's own end (a
///   one-shot `-p` run, a launch that never registered a conversation,
///   nothing readable while it ran) or no exit at all (suspended by
///   someone): the tab is left, nothing said.
/// * `Cannot` — an attempt that never can (argv that cannot resume, the
///   shell gone): said to a person.
/// * `Healthy` — the relaunched agent ran long enough: the back-off starts
///   over.
/// * `PersonReturns` — a person or a holder came back to the tab during the
///   back-off: they have it now, and nothing is typed.
///
/// `Buggy=1` is a host without the rules the owner stated (2026-09-24): it
/// relaunches after a person's own exit (`PersonWins` — a person at the
/// keyboard wins), and it drops a relaunch it cannot make, or one the owner
/// limited, without a word (`NeverSilent` — "never give up silently";
/// escalation is for what configuration limited and what is irreducible).
/// And it is the host of c4e24cd3e (`RestartCarried` — "an agent the
/// upgrade asked is never left stopped"): it asked `on_exit` about the
/// harness's own restart as about any exit, so a person's keystroke or a
/// hold left it to them (`Restarted`, `PersonReturns`), and a restart its
/// back-off let expire fell through to the graceful exit the restart's own
/// SIGTERM made (`Restarted`, `Ended`) — the agent the upgrade ended, never
/// relaunched and never said. And it is the host of 's LC-6
/// (`Land` climbing the back-off for any relaunch — `NoCrashNoStreak`: a
/// relaunch that is no crash's climbed it, so three explicit restarts made a
/// later crash wait ten minutes) and LC-4 (`PersonRuns` clearing every word
/// — `AskStandsForItsConversation`: any agent started in the tab cleared the
/// mark asking for a conversation). And it is the host of that lane before
/// its review (`TriesOnlyWhileDue`: `Ended`, `PersonReturns`, `Limited` and
/// `Cannot` each left the failing word standing, saying aterm still tries
/// over a relaunch nobody tried any more). `SaidOutlives=1`, a dial of its
/// own (the LC-4 fix it stands on is off at `Buggy=1`), is the host of the
/// harness landing's eighth merge before its review (`TriesOnlyWhileDue`
/// again: `Ended`, `PersonReturns` and the agent then run in the tab left the
/// failing said beside a word that asks for a conversation standing).
/// Tier-1 (`the_real_relaunch_decisions_conform_to_the_model`,
/// `aterm-agent`) drives the real `on_exit`, `outcome` and `Relaunches`
/// through every action, and at every state a restart holds asks the real
/// type each exit the guards above refuse it: a person, a limit, and an end
/// its carry could never answer, which the real type says as `Cannot`.
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
/// What an exit left, read as it is seen: the relaunch on exit tells a crash from a graceful
/// exit by Claude Code's own record of the agent (`sessions/<pid>.json`), which a crash cannot
/// remove and a graceful exit (a `/exit`, a `kill`) does. That record has a second writer: any
/// Claude Code that starts removes dead processes' records (Claude Code 2.1.283: a SIGKILLed
/// agent's record gone 0.79 s after the kill, as a Claude in another tab started). For one exit
/// of one agent: `exit` 0 it runs, 1 it crashed, 2 it exited gracefully; `record` its record is
/// on disk; `seen` the host's look at the exit (`relaunch::exit_record`) has decided; `kept`
/// what that look found (1: the record survived); `decided` 0 no attempt yet, 1 relaunched, 2
/// left as a graceful exit.
///
/// * `Crash`/`Graceful`: the agent ends. A graceful exit's record may still be on disk as the
///   exit is seen (it is removed in the exit's last moments, 0.02 s after the `/exit`).
/// * `OwnRemoval`: the graceful exit removes its own record.
/// * `Look`: the host's look decides and its answer is kept: at once on a removed record, else
///   once `EXIT_SETTLE` (250 ms) has passed, which outlasts an exit's own removal.
/// * `Sweep`: another Claude Code removes the dead agent's record, at any time after the look,
///   the relaunch's whole back-off (1 s to ten minutes) included. A sweep inside the settle is
///   the window this design leaves.
/// * `Decide`: an attempt (`relaunch::after_exit`) after the back-off: relaunched iff the kept
///   look found the record.
///
/// `CrashIsRelaunched`: a crash is never left as a graceful exit. `GracefulIsLeft`: a graceful
/// exit is never relaunched.
///
/// `Buggy=1` adds two hosts, each a dead action caught alone: an attempt that reads the record
/// as it stands then (`DecideAtAttempt`: `Crash`, `Look`, `Sweep`, `DecideAtAttempt` leaves a
/// crash; `CrashIsRelaunched`), and a look at the detection instant, before a graceful exit's
/// own removal (`LookAtInstant`: `Graceful`, `LookAtInstant`, `OwnRemoval`, `Decide`
/// relaunches it; `GracefulIsLeft`). Tier-1: `aterm-agent`'s
/// `the_real_exit_read_conforms_to_the_model` replays every path through the real
/// `exit_record`, `look_at_exit` and `after_exit` over a real record of a dead process, the
/// first witness through the real read at the attempt (`ExitRecord::Unread`), and an own
/// removal inside the real look's settle, which reads as `OwnRemoval` then `Look`.
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

/// A MUSE CODE EXIT, READ ON ITS SHELL'S WORD, ELSE ITS SESSION'S OWN LOG
/// (aterm-agent `upgrade_muse_drive.rs` `look_at_exit` and `after_exit`,
/// `relaunch::exit_record`; the Muse Code lane of 2026-10-17, measured on
/// 1.4.10-R6427.1). Muse Code's own table: `0` its own exit, `130`
/// an external SIGINT and `143` SIGTERM write a clean `session.end` —
/// someone's end; `129` SIGHUP and `137` SIGKILL write none — a crash; `1`
/// and `2` are a start it REFUSED (an unknown session, bad arguments), never
/// a crash. Where the shell's word cannot be read, the witnessed session's
/// own log decides: a clean `session.end` after its last start is a clean
/// end, none a crash. The session's lock is NO witness of the end: SIGTERM
/// leaves `pid=` in it as SIGKILL does, and only `/exit` truncates it.
///
/// For ONE exit of ONE Muse Code: `exit` 0 it runs, 1 it crashed, 2 its own
/// exit, 3 someone's SIGINT or SIGTERM, 4 a refused start's status;
/// `witness` the snapshot named the session its process held (its lock, held
/// open, naming its pid — no witness, no session to resume and no log to
/// read); `word` the shell has drawn the status of the block it ran in;
/// `later` a later command's block is the newest since; `seen` the look has
/// decided and `kept` what it found (1 a crash, 2 a clean end, 3 no word, 4 a
/// refusal); `told` the attempt decided on the shell's word (else on the
/// log, or nothing); `decided` 0 no attempt yet, 1 relaunched, 2 left. The
/// log is the exit's: a crash's start with no end, a clean exit's or
/// someone's signal's clean end, and a witnessed process that exited with a
/// refusal's status reads as a crash there (its start, no end).
///
/// `TheirsIsLeft`: an own exit or someone's signal is never relaunched.
/// `AToldRefusalIsNeverRetried`: a refusal the shell told is never
/// relaunched (critic X4: a relaunch would meet the same refusal).
/// `AWitnessedCrashIsRelaunched`: a crash of a witnessed session is never
/// left — its log tells it where its shell's word is gone. `NoWitnessNoRelaunch`:
/// nothing is relaunched without the session its process held.
///
/// `Buggy = 1` adds two looks and two attempts, each dead at `Buggy = 0` and
/// caught alone: the exit LOOKED AT AS CLAUDE CODE'S, its own record (the
/// lock) surviving the exit read as a crash and its removal as a clean end
/// (`LookAsClaude`: `Signal`, `LookAsClaude`, `Decide` relaunches someone's
/// SIGTERM — `TheirsIsLeft`); A REFUSAL READ AS A CRASH, a table that knows
/// only clean ends and signals, so `1` and `2` are crashes
/// (`RefusalAsCrash`: `Refuse`, `ShellWord`,
/// `RefusalAsCrash`, `Decide` retries a refused start — `AToldRefusalIsNeverRetried`);
/// an attempt that decides on the look alone (`DecideOnLook`: `Crash`,
/// `LookGone`, `ShellWord`, `DecideOnLook` leaves a crash —
/// `AWitnessedCrashIsRelaunched`); and one that relaunches a crash with no
/// witness, fresh (`DecideFresh`: `Unwitnessed`, `Crash`, `ShellWord`, `Look`,
/// `DecideFresh` — `NoWitnessNoRelaunch`). Tier-1 (aterm-agent
/// `harness/upgrade_muse_drive_tests.rs`,
/// `tier1_the_real_muse_exit_read_conforms_to_the_model`) replays every look
/// and every attempt of the model through the real `look_at_exit`,
/// `exit_record` and `after_exit` over a stand-in tab, the session's lock and
/// log on disk and the blocks drawn as the model has them.
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

/// MUSE CODE'S LIVE UPGRADE, ITS QUIET FLOW (aterm-agent
/// `harness/upgrade_muse_live.rs`, the gate and the order, and
/// `upgrade_muse_live_drive.rs`, the claim; 2026-10-18, measured on
/// 1.4.10-R6427.1). When atpkg lands a build under a supervised Muse Code, the
/// window's host restarts it onto that build at a genuinely idle point —
/// SIGTERM, then the relaunch through the managed twin on the same session —
/// and types nothing after.
///
/// The world: `landed` 0 nothing, 1 a NEWER revision build (`1.4.11-R6500.1`
/// over `1.4.10-R6427.1`), 2 one that is not newer (an older revision);
/// `turn` the session's log has a run started with no terminal (G-M1);
/// `task` a task accepted or started with no end — a backgrounded shell,
/// which a turn sends there — and `queued` its end's inbox item not yet
/// drained by Muse's own run (G-M2); `child` a descendant process (G-M3);
/// `shown` 1 where the reader does not read the screen Idle authoritatively
/// (the screen and the log move apart: a turn's screen may read idle while
/// its run is still open); `draft` a person's draft; `held` a hold, a hand
/// or a person within the grace; `unread` the log is unreadable or holds no
/// start of this process. `Restart` is the step: SIGTERM with the claim on
/// disk (`claimed`), the gate's state recorded (`at_*`); `Exit` its `143`;
/// `ReadExit` the host's read of it: `decided` 1 relaunched onto the twin (the
/// harness's own restart, `relaunch::ended_by_restart`), 2 left as someone's
/// graceful exit. `stranded`: a look at an open gate that read a newer build
/// as not newer.
///
/// `NoRunningWorkInterrupted`, `NeverMidTurn`, `NoChildKilled`: the restart
/// never ends open work, a running turn or a child. `TheRestartIsRelaunched`:
/// the exit the restart made is never left as someone's.
/// `NeverStrandedBehind`, `MovesOnlyUp`: the order reads the revision form.
///
/// `Buggy = 1` adds six mutants, each dead at `Buggy = 0` and caught alone,
/// one slip per behaviour: `RestartWithOpenWork` (a backgrounded shell or
/// its queued end read past: the screen and the run's terminal alone),
/// `RestartMidTurn` (the log's open run read past: the screen alone),
/// `RestartWithChild` (Claude Code's shell-name list, which a shell that
/// `exec`s its command slips past), `RestartUnclaimed` (the signal before
/// the claim: the `143` read as someone's end, the session left down),
/// `RevisionReadAsOlder` (the Claude lane's `upgrade::Version`, which refuses
/// `-R…`: never behind) and `RevisionReadAsNewer` (a move down). Tier-1
/// (aterm-agent `harness/upgrade_muse_live_drive_tests.rs`,
/// `tier1_the_real_muse_quiet_upgrade_conforms_to_the_model`) replays every
/// state through the real order, log reader, screen reader, process rule,
/// gate and claim.
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
            // A turn sends a shell to the background; its end queues an
            // inbox item, which Muse's own run drains.
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
            // The step: a newer build, the log closed, nothing running in it
            // or under it, the screen idle and empty, nobody holding it.
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
            // The mutants, each its own dead action.
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

/// AGY'S LIVE UPGRADE, ITS QUIET FLOW (aterm-agent `harness/upgrade_agy_live.rs`,
/// the gate and the order, and `upgrade_agy_live_drive.rs`, the witnesses and the
/// claim; 2026-10-18, measured on 1.4.0). When atpkg lands a build under a supervised
/// agy, the window's host restarts it onto that build at a genuinely idle point —
/// SIGTERM, then the relaunch through the managed twin on the same conversation — and
/// types nothing after.
///
/// The world: `landed` 0 nothing, 1 a NEWER build (`1.4.1` over `1.4.0`), 2 one that is
/// not newer; `turn` the conversation's summary row reads `RUNNING` (a turn, and a turn's
/// end a moment after the screen reads it over); `task` a background command task,
/// timer or cron runs (the screen draws it, the row is not fully idle, the transcript's
/// `RUNNING` row has no end, a command's shell is a child); `daemon` a DAEMON command
/// task runs (drawn, its `RUNNING` row open, its command a bare child — and the summary
/// row BLIND to it: measured); `subagent` a subagent works (drawn, its own row
/// `RUNNING`, its spawn row with no `has gone idle`; no process); `wake` a message that
/// wakes agy is pending (a task's end, a timer's firing, a subagent gone idle: agy's own
/// turn on it still to come); `shown` 1 where the reader does not read the screen Idle
/// authoritatively (the screen and the row move apart at a turn's end); `panel` 0 where
/// agy's setting hides the panel (`hideActiveItems`: the footer's count alone is drawn);
/// `draft` a person's draft; `held` a hold, a hand or a person within the grace;
/// `unread` the transcript cannot be read or proves no start of this process; `shared`
/// another live agy has the conversation open (its argv names it, or it holds the
/// conversation's database with no lock: the relaunch types nothing while it does).
/// The step is three acts with the world free to move between them, as the drive's
/// is: `Look` (the first look finds the gate open: `looked`), the claim written
/// (`ClaimWritten`: `claimed`, on disk) or refused (`ClaimUnwritten`: the step waits,
/// nothing signalled), then the LAST LOOK — `Restart` where the whole gate is open
/// still (SIGTERM, the world it ended recorded in `at_*`), `TakeBack` where anything
/// turned (the record put back). `Exit` is its `0`; `ReadExit` the host's read of it:
/// `decided` 1 relaunched onto the twin (the harness's own restart,
/// `relaunch::ended_by_restart`), 2 left as someone's graceful exit. `stranded`: a
/// look at an open gate that read a newer build as not newer.
///
/// `NoRunningWorkInterrupted`, `NoDaemonKilled`, `NeverMidTurn`, `NoWakeLost`: the
/// restart never ends background work, a daemon task, a running turn, or the turn agy
/// owes a wake. `NeverStrandedDown`: never an agy whose conversation another agy has
/// open (its relaunch could never type). `TheRestartIsRelaunched`: the exit the restart
/// made is never left as someone's. `NeverStrandedBehind`, `MovesOnlyUp`: the order.
///
/// `Buggy = 1` adds nine mutants, each dead at `Buggy = 0` and caught alone, one slip
/// per behaviour: `RestartWithOpenWork` (a task, a timer or a subagent read past: the
/// reader's phase and the composer alone), `RestartWithDaemonTask` (the summary row
/// trusted for background work, which a daemon task never sets, and no process rule),
/// `RestartMidTurn` (the screen read alone at a turn's end, the row still `RUNNING`),
/// `RestartWithUnreadWake` (the wake's message read past: the turn agy owes it cut off),
/// `RestartWhileShared` (another agy on the conversation read past),
/// `RestartUnclaimed` (the signal with no claim on disk — a write that failed, or none
/// made: the `0` read as someone's end, the conversation left down),
/// `RestartOnStaleLook` (the signal on the first look's word, the last look its hold
/// half alone: whatever turned in between is cut off), `BuildReadAsOlder` and
/// `BuildReadAsNewer` (the order). Tier-1 (aterm-agent
/// `harness/upgrade_agy_live_drive_tests.rs`,
/// `tier1_the_real_agy_quiet_upgrade_conforms_to_the_model`) replays every state through
/// the real order, screen reader, summary rule, transcript reader, process rule, wake
/// rule, sharers, gate and claim.
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
            // A turn starts background work, which runs on after it ends; its
            // end writes a message that wakes agy, whose own turn takes it.
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
            // Another agy opens the conversation (`agy -c` in another tab)
            // and closes it.
            action SharedOpens when (moved == 0 && shared == 0) {
                shared = 1;
            }
            action SharedCloses when (moved == 0 && shared == 1) {
                shared = 0;
            }
            // The step's first look: a newer build, no turn by the row,
            // nothing running in or under it, no wake pending, the screen idle
            // and empty, nobody holding it, the transcript read, no other agy
            // on the conversation.
            action Look when (
                moved == 0 && looked == 0 && landed == 1 &&
                turn == 0 && task == 0 && daemon == 0 && subagent == 0 && wake == 0 &&
                shown == 0 && draft == 0 && held == 0 && unread == 0 && shared == 0
            ) {
                looked = 1;
            }
            // The claim, on disk before anything else — or refused.
            action ClaimWritten when (moved == 0 && looked == 1 && claimed == 0) {
                claimed = 1;
            }
            action ClaimUnwritten when (moved == 0 && looked == 1 && claimed == 0) {
                looked = 0;
            }
            // The last look: the whole gate again, the claim on disk.
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
            // The mutants, each its own dead action.
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

/// AN AGY EXIT, READ ON ITS SHELL'S WORD AND ITS CRASH FILE (aterm-agent
/// `upgrade_agy_drive.rs` `look_at_exit` and `after_exit`,
/// `relaunch::exit_record`; the agy lane of 2026-10-17, measured on 1.4.0).
/// agy's own table: `0` every graceful way out — its own `/quit`, Ctrl-C or
/// Ctrl-D twice, and someone's SIGTERM, SIGHUP or SIGINT, mid-turn too —
/// removes its crash file (`<config>/crashes/crash_<pid>_<uuid>.log`);
/// SIGKILL `137` leaves it; Ctrl-Z `146` (or another stop signal's status)
/// STOPS agy, alive, its lock and crash file held; a start agy refuses exits
/// `2` with no crash file; a Go panic is UNMEASURED — its status and whether
/// its crash file survives — and is modelled as `2` beside its crash file
/// (the table reads the file, whichever: a panic that removes it reads as a
/// refusal, never relaunched). So any status but `0` and a stop's is a crash
/// only where the dead pid's crash file is left — and
/// the NEXT agy start deletes a dead pid's file, so the exit's look reads it
/// as the exit is seen and keeps it. The conversation's presence lock is NO
/// witness of the end: the file is never removed.
///
/// For ONE exit of ONE agy: `exit` 0 it runs, 1 SIGKILL, 2 a graceful way
/// out, 3 Ctrl-Z (alive), 4 a refused start, 5 a panic; `witness` the
/// snapshot named the conversation its process held (its presence lock held
/// open — no witness, nothing to resume); `word` the shell has drawn the
/// status of the block it ran in; `later` a later command's block is the
/// newest since; `swept` another agy's start deleted the dead pid's crash
/// file; `filed` the crash file stood as a look read the shell's word (a fact
/// of the world, set by every look alike); `seen` the look has decided and
/// `kept` what it found (1 a crash, 2 a clean end, 3 no verdict — read again
/// at the attempt — 4 a refusal); `decided` 0 no attempt yet, 1 relaunched,
/// 2 left. The world stands still once decided.
///
/// `TheirsIsLeft`: a graceful way out is never relaunched. `StoppedIsLeft`:
/// a Ctrl-Z'd agy is never relaunched. `ARefusalIsNeverRetried`: a refused
/// start is never relaunched. `ACrashReadAtItsExitIsRelaunched`: a witnessed
/// crash whose crash file stood as its exit was read is never left.
/// `NothingRelaunchedOnAGuess`: a relaunch had the crash file before it, at
/// the look or at the attempt. `NoWitnessNoRelaunch`: nothing is relaunched
/// without the conversation its process held.
///
/// `Buggy = 1` adds three looks and two attempts, each dead at `Buggy = 0`
/// and caught alone: the exit LOOKED AT AS CLAUDE CODE'S, its own record (the
/// presence lock) surviving every exit read as a crash (`LookAsClaude`:
/// `Graceful`, `ShellWord`, `LookAsClaude`, `Decide` relaunches it —
/// `TheirsIsLeft`); a REFUSAL READ AS A CRASH, the status `2` alone
/// (`RefusalAsCrash`: `Refuse`, `ShellWord`, `RefusalAsCrash`, `Decide` —
/// `ARefusalIsNeverRetried`); THE CRASH FILE IGNORED, the status alone (`137`
/// a crash, `2` a refusal: `CrashFileIgnored`: `Crash`, `Sweep`, `ShellWord`,
/// `CrashFileIgnored`, `Decide` relaunches a crash no file witnesses —
/// `NothingRelaunchedOnAGuess` — and a panic's `2` beside its file is left —
/// `ACrashReadAtItsExitIsRelaunched`); CTRL-Z READ AS A CRASH, an attempt
/// that reads the stopped agy's `146` and the crash file it still holds as a
/// crash, never asking whether it lives (`StopAsCrash`: `Stop`, `ShellWord`,
/// `LookGone`, `StopAsCrash` — `StoppedIsLeft`); and one that relaunches a
/// crash with no witness, fresh (`DecideFresh`: `Unwitnessed`, `Crash`,
/// `ShellWord`, `Look`, `DecideFresh` — `NoWitnessNoRelaunch`). Tier-1
/// (aterm-agent `harness/upgrade_agy_drive_tests.rs`,
/// `tier1_the_real_agy_exit_read_conforms_to_the_model`) replays every look
/// and every attempt of the model through the real `look_at_exit`,
/// `exit_record` and `after_exit` over a stand-in tab, the blocks and the
/// crash file on disk as the model has them, and requires each mutant caught
/// on its own.
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

/// A GEMINI CLI EXIT, READ ON ITS SHELL'S WORD, ITS SESSION NAMED BY THE
/// START-TIME RULE AND ITS RESUME PRE-CHECKED (aterm-agent
/// `upgrade_gemini_drive.rs` `look_at_exit` and `after_exit_onto`,
/// `relaunch::exit_record`; the Gemini CLI lane of 2026-10-18, measured on
/// 0.46.0). Gemini CLI's own table: `0` every graceful way out (`/quit`,
/// `/exit`, Ctrl-C or Ctrl-D twice, SIGTERM to its child); `1` its child
/// killed and `137` its parent killed — crashes; `41`, `42`, `52` a start it
/// refused (no credential; an unknown id, another project's session, a
/// duplicate id; its config) — never a crash; any other status names nothing
/// measured (the parent ignores SIGTERM, SIGHUP and SIGINT). But a `42` from
/// a launch under its own `--session-id` whose session the start-time rule
/// names is ITS OWN RESTART (`/settings`, IDE trust, the folder-trust
/// dialog: exit `199`, its argv run again) met by the id its first start
/// wrote (`ExitKind::Restarted`, measured with and without a first turn):
/// resumed on that session by id as a crash is, and quietly — nothing typed
/// after it. No file names a process's session and none is held open, so its
/// session is the ONE session header of its project begun since it started,
/// within the window its own lands in (each launch, each resume's duplicate
/// and each `/clear` writes one), while no other session's file moved since:
/// anything else names none. And its own `--resume` loads the newest of the
/// session's files, which after two resumes with no turn between is an empty
/// duplicate: the resume is typed only where a read-only fold of those files
/// says it loads every turn. A session that took no turn is resumed only on
/// its own restart of a launch that carried no first prompt (the measured
/// folder-trust restart before any turn); a crash's leaves nothing to bring
/// back, and a restart's whose first prompt was never sent would drop it.
///
/// For ONE exit of ONE Gemini CLI: `exit` 0 it runs, 1 its child killed
/// (`1`), 2 a graceful way out, 3 a refused start, 4 a status with no word in
/// the table, 5 its parent killed (`137`), 6 its own restart refused for the
/// `--session-id` its first start wrote (`42`); `rule` what the start-time
/// rule sees — 0 its own header alone since its start, 1 a second header
/// since (a `/clear`, another launch), 2 its own header never written and
/// another launch's begun past the window, 3 an in-TUI `/resume` onto an
/// older session (a switch onto it, no header) with a start's background
/// summary written after the switch; `dup` its resume would load an empty
/// duplicate; `turnless` the session took no turn in any of its files;
/// `prompt` the launch carried a first prompt (every worker's launch is
/// under its own `--session-id`, prompted or not); `word` the shell has
/// drawn the status of the block it ran in; `later` a later command's block
/// is the newest since; `seen` the look has decided and `kept` what it found
/// (1 a crash, 2 a clean end, 3 no verdict — read again at the attempt — 4
/// a refusal, 5 its own restart: `relaunch::ExitRecord::Restarted`, the
/// word that told it kept, as the shell may draw it no more by the
/// attempt); `decided` 0 no attempt yet, 1 relaunched, 2 left (told, never
/// typed); `cont` the relaunch typed its continuation after the line. The
/// world stands still once decided.
///
/// `TheirsIsLeft`: a graceful way out is never relaunched.
/// `ARefusalIsNeverRetried`: a refused start is never relaunched.
/// `NothingRelaunchedOnAGuess`: only a crash, or its own restart, is
/// relaunched. `ACrashReadAtItsExitIsRelaunched`: a crash whose shell drew
/// its word — at its exit, or still at the attempt — its session named,
/// holding a turn and its resume safe, is never left.
/// `AnOwnRestartIsResumed`: its own restart so read, its session named and
/// its resume safe, is never left — with no turn too, but for a launch whose
/// first prompt would be dropped. `ARestartTypesNothingAfter`:
/// its own restart's relaunch types no continuation.
/// `ACrashRelaunchedIsContinued`: a crash's relaunch types its continuation
/// after its line. `NoUniqueWitnessNoRelaunch`:
/// nothing is relaunched where the start-time rule names no one session.
/// `NeverIntoAnEmptyCopy`: nothing is relaunched where its resume would load
/// a copy missing its turns. `NoTurnResumedButABareRestart`: a session that
/// took no turn is relaunched only on its own restart of a launch with no
/// first prompt.
///
/// `Buggy = 1` adds four looks and eight attempts, each dead at `Buggy = 0`
/// and caught alone: the exit LOOKED AT AS CLAUDE CODE'S, its own record (its
/// session file, which every way out leaves) surviving read as a crash
/// (`LookAsClaude`: `Graceful`, `ShellWord`, `LookAsClaude`, `Decide`
/// relaunches it — `TheirsIsLeft`); a REFUSAL READ AS A CRASH, a table that
/// knows only clean ends and crashes (`RefusalAsCrash`: `Refuse`, `ShellWord`,
/// `RefusalAsCrash`, `Decide` — `ARefusalIsNeverRetried`); its dual, A
/// CHILD'S DEATH READ AS A REFUSAL, Muse Code's table (whose `1` is a start
/// it refused) on Gemini CLI's (whose `1` is its child killed:
/// `ChildDeathAsRefusal`: `Crash`, `ShellWord`, `ChildDeathAsRefusal`,
/// `Decide` leaves it — `ACrashReadAtItsExitIsRelaunched`); A RESTART READ AS
/// A REFUSAL, the status alone (`RestartAsRefusal`: `Restart`, `ShellWord`,
/// `RestartAsRefusal`, `Decide` leaves it — `AnOwnRestartIsResumed`); A
/// NON-UNIQUE WITNESS TRUSTED, the first header since its start read as its
/// session whatever came after (`NonUniqueTrusted`: `SecondHeader`, `Crash`,
/// `ShellWord`, `Look`, `NonUniqueTrusted` — `NoUniqueWitnessNoRelaunch`); A
/// LATE HEADER TRUSTED, the one header since its start read as its own
/// however long after it was begun (`LateHeaderTrusted`: `LateHeader`,
/// `Crash`, `ShellWord`, `Look`, `LateHeaderTrusted` —
/// `NoUniqueWitnessNoRelaunch`); A SWITCH HIDDEN BY A SUMMARY, an in-TUI
/// `/resume` read only as a file's last record, so the summary after it
/// hides it (`SwitchHiddenBySummary`: `SwitchedBehindSummary`, `Crash`,
/// `ShellWord`, `Look`, `SwitchHiddenBySummary` —
/// `NoUniqueWitnessNoRelaunch`); THE PRE-CHECK SKIPPED, the resume typed
/// whatever it would load (`PrecheckSkipped`: `Duplicate`, `Crash`,
/// `ShellWord`, `Look`, `PrecheckSkipped` — `NeverIntoAnEmptyCopy`); A
/// TURNLESS RESTART LEFT, the pre-check's no turn read as nothing to resume
/// for its own bare restart too — the lane before 2026-10-21, which left the
/// measured folder-trust restart at Gemini CLI's error (`TurnlessRestartLeft`:
/// `NoTurn`, `Restart`, `ShellWord`, `Look`, `TurnlessRestartLeft` —
/// `AnOwnRestartIsResumed`); NO TURN RESUMED, a session with no turn resumed
/// whatever ended it (`TurnlessResumed`: `NoTurn`, `Crash`, `ShellWord`,
/// `Look`, `TurnlessResumed` — `NoTurnResumedButABareRestart`); A GONE WORD
/// READ AS A RESTART, the lane of 3a4586a01e: a crash whose word another
/// block hid by the attempt resumed as its own restart wherever its `42`
/// would have been one — every worker's `--session-id` launch — quietly,
/// and with no turn too (`GoneWordReadAsRestart`: `Crash`, `ShellWord`,
/// `Look`, `Later`, `GoneWordReadAsRestart` — `ACrashRelaunchedIsContinued`;
/// the review of 3a4586a01e, R2); and A RESTART CONTINUED, its relaunch
/// typing a crash's continuation into the session a person restarted
/// (`RestartContinued`: `Restart`, `ShellWord`, `Look`, `RestartContinued` —
/// `ARestartTypesNothingAfter`). Tier-1 (aterm-agent
/// `harness/upgrade_gemini_drive_tests.rs`,
/// `tier1_the_real_gemini_exit_read_conforms_to_the_model`) replays every look
/// and every attempt of the model through the real `look_at_exit`,
/// `exit_record` and `after_exit` over a stand-in tab, the blocks and the
/// project's session files on disk as the model has them, and requires each
/// mutant caught on its own.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_gemini_exit_witness_model() -> Model {
    aterm_spec::ty_model! {
        HarnessGeminiExitWitness {
            const Buggy = 0;
            var exit = 0;
            var rule = 0;
            var dup = 0;
            var turnless = 0;
            var prompt = 0;
            var word = 0;
            var later = 0;
            var seen = 0;
            var kept = 0;
            var decided = 0;
            var cont = 0;

            action Crash when (exit == 0) {
                exit = 1;
            }
            action Graceful when (exit == 0) {
                exit = 2;
            }
            action Refuse when (exit == 0) {
                exit = 3;
            }
            action Unwitnessed when (exit == 0) {
                exit = 4;
            }
            action ParentKilled when (exit == 0) {
                exit = 5;
            }
            action Restart when (exit == 0) {
                exit = 6;
            }
            action SecondHeader when (exit == 0 && rule == 0) {
                rule = 1;
            }
            action LateHeader when (exit == 0 && rule == 0) {
                rule = 2;
            }
            action SwitchedBehindSummary when (exit == 0 && rule == 0) {
                rule = 3;
            }
            action Duplicate when (exit == 0 && dup == 0) {
                dup = 1;
            }
            action NoTurn when (exit == 0 && turnless == 0) {
                turnless = 1;
            }
            action Prompted when (exit > 0 && prompt == 0 && seen == 0) {
                prompt = 1;
            }
            action ShellWord when (exit > 0 && word == 0 && decided == 0) {
                word = 1;
            }
            action Later when (word == 1 && later == 0 && decided == 0) {
                later = 1;
            }
            action Look when (exit > 0 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = if (exit == 1 || exit == 5) {
                    1
                } else {
                    if (exit == 6 && rule == 0) {
                        5
                    } else {
                        if exit == 2 {
                            2
                        } else {
                            if (exit == 3 || exit == 6) { 4 } else { 3 }
                        }
                    }
                };
            }
            action LookGone when (exit > 0 && seen == 0 && (word == 0 || later == 1)) {
                seen = 1;
                kept = 3;
            }
            action LookAsClaude when (Buggy == 1 && exit > 0 && seen == 0) {
                seen = 1;
                kept = 1;
            }
            action RefusalAsCrash when (Buggy == 1 && exit == 3 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = 1;
            }
            action ChildDeathAsRefusal when (Buggy == 1 && exit == 1 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = 4;
            }
            action RestartAsRefusal when (Buggy == 1 && exit == 6 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = 4;
            }
            action Decide when (seen == 1 && decided == 0) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && rule == 0 && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0))) {
                    1
                } else {
                    2
                };
                cont = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && rule == 0 && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0)) && exit <= 5) { 1 } else { 0 };
            }
            action NonUniqueTrusted when (Buggy == 1 && seen == 1 && decided == 0 && rule == 1) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0))) {
                    1
                } else {
                    2
                };
            }
            action LateHeaderTrusted when (Buggy == 1 && seen == 1 && decided == 0 && rule == 2) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0))) {
                    1
                } else {
                    2
                };
            }
            action SwitchHiddenBySummary when (Buggy == 1 && seen == 1 && decided == 0 && rule == 3) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0))) {
                    1
                } else {
                    2
                };
            }
            action PrecheckSkipped when (Buggy == 1 && seen == 1 && decided == 0 && dup == 1) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && rule == 0 && (turnless == 0 || (exit == 6 && prompt == 0))) {
                    1
                } else {
                    2
                };
            }
            action TurnlessRestartLeft when (Buggy == 1 && seen == 1 && decided == 0 && exit == 6 && turnless == 1) {
                decided = 2;
            }
            action TurnlessResumed when (Buggy == 1 && seen == 1 && decided == 0 && turnless == 1) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && rule == 0) {
                    1
                } else {
                    2
                };
            }
            action GoneWordReadAsRestart when (Buggy == 1 && seen == 1 && decided == 0 && kept == 1 && later == 1) {
                decided = if (rule == 0 && (dup == 0 || turnless == 1) && (turnless == 0 || prompt == 0)) {
                    1
                } else {
                    2
                };
                cont = 0;
            }
            action RestartContinued when (Buggy == 1 && seen == 1 && decided == 0 && exit == 6) {
                decided = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && rule == 0 && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0))) {
                    1
                } else {
                    2
                };
                cont = if ((kept == 1 || kept == 5 || (kept == 3 && word == 1 && later == 0 && (exit == 1 || exit == 5 || exit == 6))) && rule == 0 && (dup == 0 || turnless == 1) && (turnless == 0 || (exit == 6 && prompt == 0))) { 1 } else { 0 };
            }

            invariant TheirsIsLeft: exit <= 1 || exit > 2 || decided == 0 || decided == 2;
            invariant ARefusalIsNeverRetried: exit <= 2 || exit > 3 || decided == 0 || decided == 2;
            invariant NothingRelaunchedOnAGuess: decided == 0
                || decided == 2
                || exit == 1
                || exit == 5
                || exit == 6;
            invariant ACrashReadAtItsExitIsRelaunched: exit == 0
                || exit == 2
                || exit == 3
                || exit == 4
                || exit == 6
                || ((kept <= 0 || kept > 1) && (word == 0 || later == 1))
                || rule > 0
                || dup == 1
                || turnless == 1
                || decided <= 1;
            invariant AnOwnRestartIsResumed: exit <= 5
                || ((kept <= 4 || kept > 5) && (word == 0 || later == 1))
                || rule > 0
                || (dup == 1 && turnless == 0)
                || (turnless == 1 && prompt == 1)
                || decided <= 1;
            invariant ARestartTypesNothingAfter: exit <= 5 || cont == 0;
            invariant ACrashRelaunchedIsContinued: exit == 0
                || exit == 2
                || exit == 3
                || exit == 4
                || exit == 6
                || decided == 0
                || decided == 2
                || cont == 1;
            invariant NoUniqueWitnessNoRelaunch: rule == 0 || decided == 0 || decided == 2;
            invariant NeverIntoAnEmptyCopy: dup == 0 || turnless == 1 || decided == 0 || decided == 2;
            invariant NoTurnResumedButABareRestart: turnless == 0
                || decided == 0
                || decided == 2
                || (exit == 6 && prompt == 0);
        }
    }
}

/// The upgrade's looks at one session, as its worker takes them at the loop's idle points
/// (`aterm-gui`'s `harness_host::WorkerIdle::at_idle`, over `aterm-agent`'s
/// `upgrade_drive::due`), and its note behind (`harness_host::ask_note`, over
/// `upgrade_drive::note_behind`), from the worker's attach at a fresh launch. For one session:
/// `due` it has an upgrade to take; `readable` it can be read (the instance's socket answers,
/// Claude Code's record of the agent is written, a moment after the attach at a launch);
/// `ready` it answered READY, so its next step restarts it; `looks` a look is owed (the loop
/// parked for the next idle point, or a later look set on the ladder); `done` the upgrade said
/// its last word; `owed` its note behind is still to be taken; `noted` its upgrade state is
/// minted, and `aged` with its age from the attach.
///
/// * `Look`: a look that reads an upgrade to take asks the owed note first, then steps it (the
///   notice with the READY answer folded in, then the restart and the last word). A step that
///   finds no state mints one, aged from the step.
/// * `NotDue`: a look that reads nothing to take lets the upgrade go.
/// * `Unread`: a look that could not read says so (`wait:no-socket`, `wait:no-record`) and sets
///   a later look on the ladder.
/// * `Note`/`NoteUnread`: the owed note asked (at the attach, the host's wakes and ladder, an
///   idle point): read, it mints the state with the attach's age or finds nothing to note;
///   unread, it stays owed.
/// * `Lose`/`Regain`: the reads fail and come back.
/// * `Settle`: the upgrade stops being due by another hand (the owner's `--skip`, a person's
///   own restart onto the build).
///
/// `NeverDropped`: an upgrade due and not done always has a look owed, one that answered READY
/// included. `BehindFromTheAttach`: a minted state is behind from the attach, not from a later
/// first step.
///
/// `Buggy=1`: an unreadable look owes nothing (`Regain`, `Look`, `Lose`, `Unread`: a READY'd
/// session let go), and an unreadable note is given up (`NoteUnread`, `Regain`, `Look`).
/// Tier-1, in two halves: `aterm-gui`'s `the_real_look_conforms_to_the_upgrade_look_model`
/// drives the real `attach`, `WorkerIdle` and `ask_note` through every transition from every
/// reachable state, projecting the worker's park, later look and owed note onto `looks` and
/// `owed`; `aterm-agent`'s `the_real_classifier_conforms_to_the_upgrade_look_model` drives the
/// real `upgrade_drive::due_among` and `no_record` over every combination of what a look reads
/// of a tab, projecting each onto `due` and `readable` (every read the verdict needed), and
/// checks the model enables the action its verdict is.
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

/// An age belongs to one process: a record minted behind one process must not lend its age to
/// another (after a hand restart onto the newest build and then a newer build, the new process
/// would be judged by the old record's days). For one conversation: `proc` the process that
/// holds it (0 the one the record was minted behind, 1 after the owner's restart by hand); `rec`
/// the record (0 none, 1 counts process 0's age, 2 process 1's); `behind` the holder runs a
/// build older than the newest; `aged` the record's age is past the judging bound
/// (`STALLED_AFTER_S`); `alarm` a judgment by that age was made. The session starts behind, its
/// record minted for process 0.
///
/// * `Time`: the bound passes on the record.
/// * `HandRestart`: the owner restarts the agent by hand onto the newest build: a new process,
///   no longer behind; the record is the old process's.
/// * `Visit`: the next step finds nothing to move (`current`) and closes a record for a build
///   it now runs (`forget_settled_pending`).
/// * `NewBuild`: a newer build lands; the record (or, none standing, the one the next step
///   mints) carries its age only while it counts this process (`St::for_target`).
/// * `Judge`: something reads a behind session's record age and alarms past the bound.
///
/// `AgeIsThisProcess`: no judgment by an age that is not the holder's. `Buggy=1`: the record
/// survives the hand restart (`Visit` closes nothing) and the retarget carries whatever age
/// stands. Tier-1: `aterm-agent`'s `the_real_age_conforms_to_the_age_model` drives the real
/// `St::for_target`, `forget_settled_pending` and the owner's view over every reachable state.
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

/// The explicit restart (`docs/HANDOFF-robustness-rounds-2026-10-08.md` §4):
/// `aterm ctl @<sid> restart [pid=<n>] [if-stalled]` writes its record before its signal, as
/// every in-place restart of the harness does (the live upgrade, the memory, model and account
/// restarts), and the exit is recognised by lookup, never by inference.
///
/// For one process `P` holding one tab, and any number of acts on it, one at a time
/// (`restart_act::ACTING`): `alive` `P` runs; `stamped` a stamp naming `P` (pid and kernel
/// start) is on disk (`<harness state>/upgrade/asked-<tab>.json`, written atomically);
/// `inflight` an act has stamped and not yet signalled or given up; `fresh` that act wrote the
/// stamp (else it joined the one standing); `termed` an act's `SIGTERM` was sent; `esc` the
/// escalation watches running on `P` in the instance that made the act, one at most (an act
/// that finds one running gives it its stamp's time instead, `restart_act::watch`);
/// `committed` a seamless update's Commit handed the session to a successor (the outgoing
/// instance's threads end with it; the stamp on disk does not); `ended` who ended `P` (0 nobody
/// yet, 1 an act: its `TERM`, its `KILL` or the carrier's, 2 anyone else: a person's `/exit`, a
/// crash, a shell's `kill`); `at_end` the stamp stood when `P` ended; `read` the exit's reader
/// (the worker, the exit-only worker, or a successor's): 0 not yet, 1 relaunched as the act's
/// (`ExitCause::Asked`/`Stall`, a person's keystroke before it not read as their `/exit`), 2
/// read by the other rules (a graceful exit is left), 3 left to another supervisor that claims
/// the session, 4 left by aterm's copy of a self-claimed agent's record (`CopyLeaves`); `hand`
/// another hand on the session, what the act's `KILL` waits out
/// (`restart_act::another_hand`): 0 none, 1 one the relaunch waits out too (a hold, a driver's
/// lease or turn: `relaunch::hand_on_tab`), 2 another supervisor's claim, which takes the exit
/// whether or not such a hand stands beside it (the reader asks the claim first, at every look
/// of a carry: `claimed_by_another`); `held_kill` an act's `KILL` was sent while another hand
/// held the session; `into_claim` a relaunch was typed into a tab another supervisor claims
/// (the relaunch line does not wait on a claim).
///
/// * `Stamp`: an act's first half (`restart_act::begin`), while `P` leads and no other hand
///   holds the session: the stamp written, or, one naming `P` standing, joined (`fresh` 0).
/// * `Term`: its second half: the discard and `killpg(SIGTERM)` while `P` still leads (a
///   re-read), and the one watch on `P` started or, one running, kept. `SendsNothing`: the
///   re-read found `P` gone (or `killpg` refused): nothing sent, and a stamp the act wrote
///   taken back (`restart_act::take_back`); a joined one is kept.
/// * `Hears` — `P` runs its `SIGTERM` handler and exits; a deaf `P` (its
///   JS thread spinning, `trap '' TERM`) never does.
/// * `Escalate` — an act's thread, `P` still leading after the grace and no
///   other hand on the session (re-read right before the discard and the
///   `killpg`, ruling 11): `killpg(SIGKILL)`, its `held_kill` the hand it was
///   sent under. `EscalationSeesExit` — its wait on `NOTE_EXIT` ends.
/// * `EndsOtherwise` — anyone ends `P`, at any point.
/// * `HandOn` / `ClaimOn` / `HandOff` — another hand lands on the session,
///   or leaves it, at any point: a fleet halt, the bridge-lost auto-hold,
///   the owner's local hold, a driver's lease or turn (`HandOn`), or another
///   supervisor's claim (`ClaimOn`) — on a free session or
///   beside a hand already standing, an end carried through it included (the
///   review of ruling 20); `HandOff` lifts what stands.
/// * `Commit` — at any point; `CarrierFinishes` — the successor's carrier
///   finds a stamp naming a live leader past the act's 30 s and finishes the
///   `KILL` (it holds the master now), no other hand on the session (its
///   `held_kill` as `Escalate`'s).
/// * `Read` — the exit is read, by whichever reader, and the stamp it finds
///   naming `P` is consumed: once `P` has ENDED — an exit whose program still
///   runs (stopped away from its tab, its shell in front) is nobody's yet,
///   decided at its end as an exit's first read is, the stamp looked up and
///   another supervisor's claim asked again there (rulings 15 and 17) — and
///   with no other hand on the session: an end the act's stamp names, read
///   while a hold, a lease or a named turn holds it, is CARRIED through that
///   hand and relaunched once it lifts (ruling 20 of 2026-10-14,
///   `relaunch::WAIT_HAND_LIFTS`), which is this `Read` after `HandOff`; its
///   `into_claim` is the claim standing at it, none. `ReadByClaim` — an end
///   read while another supervisor claims the session is that supervisor's
///   (`claimed_by_another`, ruling 17) — one carried through a hold when the
///   claim landed included, asked at every look of the carry (the review of
///   ruling 20): left to it, the stamp unconsumed, `read` 3.
///
/// Invariants: `TheActsEndIsRelaunched` (an end an act made is relaunched whoever reads it,
/// however many acts there were; or, read while another live supervisor claims the session,
/// left to it, `read` 3 with `by` 2, since a proven self-claim whose process ended relaunches
/// nothing; or left by the agent's own copy, said once, `read` 4: it recorded an orderly end
/// or names no line that is the live process's, and no line read from a writable file is typed
/// unless the agent ran it); `OnlyAStampedEndIsTheActs` (nothing is relaunched as the act's
/// unless the stamp stood at its end); `TheActEndsItsProgram` (a `P` an act signalled is never
/// left alive with nobody to end it: its thread, or after a Commit the carrier);
/// `TheKillWaitsOutAnotherHand` (no act's `KILL` lands while another hand holds the session:
/// the halt contract); `OneWatchPerProcess` (one watch ends `P` however many acts there were:
/// each watch holds a kqueue, a descriptor against the window's 256);
/// `NothingIsTypedIntoAClaim`. Liveness: [`harness_restart_stamp_liveness`].
///
/// * `Copied` / `CopyRelaunches` / `CopyLeaves` — ATERM'S COPY of the
///   self-claimed agent's record (`copy`, read while the proven claim stood
///   and `P` ran). Where it stands, it is the end's ONE DECIDER (the lane's
///   `Read` does not run): it relaunches the end on the line it validated
///   (as the act's when the stamp stood, consuming it), or LEAVES it, said
///   once — its verdict orderly (the record removed, an exit word that is
///   not aterm's signal) or its line refused against the live process
///   (`read` 4). A claim given back while `P` runs (`SelfClaimOff`, passive)
///   drops the copy: `P`'s later end is never decided from a copy frozen at
///   the release. A seamless update's `Commit` while `P` runs carries
///   neither the proven claim nor the copy (the successor re-proves it,
///   `claim_known=false`; the copy is the outgoing instance's memory). For a
///   codex-alab, either end is NOBODY'S (below); a
///   `Commit` once `P` has ended carries the copy's decision — the relaunch
///   it owes rides the handoff as the owed relaunch, on the line the copy
///   validated (`relaunch::CodexRun::self_claimed`, amended 2026-10-16 for
///   the Codex lane's retirement).
///
/// * `IsAlab` / `Unclaimed` — `P` IS CODEX-ALAB (`alab`; the Codex screen
///   lane's retirement, 2026-10-16: aterm supervises no Codex, and the host
///   admits a Codex only as codex-alab). Its end is never the lane's `Read`:
///   with the copy standing the copy decides; with none (never proven, given
///   back while it ran, or a Commit while it ran) it is `Unclaimed` — left,
///   said once, whatever ended it (`harness_host::unproven_codex_exit`; `read`
///   4 for a stamped end, 2 for its own) — and no act is made of it with no
///   copy to bring it back (`Stamp` waits for `copy == 1`: the act answers
///   `not-relaunchable`, `restart_act::unavailable`). `lane` — a codex-alab's
///   end read by the lane.
///
/// Invariants: `TheActsEndIsRelaunched` — an end an act made is relaunched
/// whoever reads it, however many acts there were — or, read while another
/// LIVE supervisor claims the session, left to that supervisor (`read` 3
/// with `by` 2, ruling 17: a supervisor of the session relaunches what it
/// holds; a proven self-claim whose process ended relaunches nothing, so it
/// takes no end), or left by the agent's own copy, said once (`read` 4: it
/// recorded an orderly end, or names no line that is the live process's —
/// no line read from a writable file is typed unless the agent ran it);
/// `TheCopysDecisionRidesTheCommit` — an end the copy is to decide is never
/// lost to a seamless update's `Commit` (`lost`: the copy dropped after
/// `P` ended, the end left to the lane's `Read`, which never saw the line);
/// `NoCodexEndIsTheLanes` — no codex-alab's end is read or relaunched by the
/// lane (`lane`: no line read off a shell's word or a writable file is typed
/// for an agent aterm does not supervise);
/// `OnlyAStampedEndIsTheActs`
/// — nothing is relaunched as the act's unless the stamp stood at its end;
/// `TheActEndsItsProgram` — a `P` an act signalled is never left alive with
/// nobody to end it (its thread, or after a Commit the carrier);
/// `TheKillWaitsOutAnotherHand` — no act's `KILL` lands while another hand
/// holds the session (the halt contract; under the act's old `KILL` the
/// reader then left the agent the act had killed at a bare shell);
/// `OneWatchPerProcess` — however many acts there were, one watch ends `P`
/// (bug hunt 7: every joined act's thread held a kqueue of its own, a
/// descriptor per request against the window's 256);
/// `NothingIsTypedIntoAClaim` — no relaunch is typed into a tab another
/// supervisor claims (ruling 17's blocker: it relaunches what it holds).
/// Liveness
/// ([`harness_restart_stamp_liveness`]): a stamped `P` ends, once every
/// other hand is released.
///
/// Not modelled: the stamp's clock. The watch lets a program go once its stamp no longer
/// stands (`STALE_S`: stopped away from its tab, or held by another hand that outlasts it); no
/// `KILL` is sent for a stamp that no longer stands, nor in its last half minute; an act that
/// joins a stamp renews its time once its own `TERM` is sent; and an end after the stamp went
/// stale is the program's own. Saying it needs a stale flag threaded through every end, a
/// weakened relaunch invariant and a record of a `KILL` apart from the program's own end. Nor
/// what the watch reads before its `KILL` (it lets its program go once the tab's stamp names
/// another, and sends it only once the host holds the snapshot the exit's reader reads it by):
/// a second process and the reader's snapshot, which this model lacks. `aterm-gui`'s
/// `a_watch_lets_its_program_go_once_its_tabs_stamp_names_another`,
/// `a_program_stopped_across_a_commit_is_ended_once_its_successor_read_it`,
/// `a_stamped_program_stopped_inside_its_grace_is_ended_once_it_leads_again`,
/// `a_stamp_that_no_longer_stands_is_no_acts_to_finish`,
/// `a_restart_after_its_programs_watch_let_go_gets_a_watch_of_its_own`
/// and `a_stopped_programs_end_after_its_stamp_went_stale_is_its_own` pin it
/// on the real code. Nor a STOPPED `P` as a state of its own: `alive` covers it,
/// and `Read`'s `alive == 0` is what the reader must wait for (ruling 15).
///
/// `Buggy=1` adds thirteen mutants, each caught alone: `SignalBeforeStamp`
/// (the signal sent first: a reader of the exit before the stamp lands reads
/// a graceful exit — `TheActsEndIsRelaunched`), `CommitForgetsStamp` (the
/// record in the instance's memory, `RemedySent`'s, lost across a Commit —
/// `TheActsEndIsRelaunched`), `TermOnly` (no escalation: a program that lives
/// through `TERM` survives the act, and a person's later `/exit` would be
/// relaunched as the act's — `TheActEndsItsProgram`, and the liveness),
/// `StampAfterEnd` (a stamp written for a process that already ended — the
/// re-read of step 1d missing — makes a person's exit the act's —
/// `OnlyAStampedEndIsTheActs`), `TakeBackAlways` (an act that sent nothing
/// takes back the stamp whatever wrote it — 3eaa8f83c's second act, which
/// replaced the first's stamp, found `P` gone and removed the only stamp the
/// first act's exit had — `TheActsEndIsRelaunched`), `KillUnderHand`
/// (1a733ed60's `KILL`, which re-read only the leader: a hold that landed in
/// the grace did not stop it — `TheKillWaitsOutAnotherHand`) and
/// `DecidedWhileRunning` (the reader settles an exit whose program still
/// runs — 1a733ed60's `not-exited` read as left, and the review's second
/// round's `Held` under a hand that came and went while `P` was stopped —
/// and the end the act makes after `fg` is nobody's to relaunch —
/// `TheActsEndIsRelaunched`), `WatchPerAct` (an act whose `SIGTERM` is
/// sent starts a watch of its own beside the one running — the act before
/// bug hunt 7 — `OneWatchPerProcess`), `LeftToTheHolder` (the reader
/// before ruling 20: an end read while a hold, a lease or a named turn holds
/// the session left to that holder, the agent a person asked to restart at
/// its shell — `TheActsEndIsRelaunched`) and `CarriedIntoAClaim` (ruling
/// 20's first reader, which asked the claim only at an exit's first read: a
/// claim that landed while the end was carried through a hold was asked at
/// no later look, and the relaunch was typed into the tab it claims once the
/// hold lifted — `NothingIsTypedIntoAClaim`), and `DeadSelfClaimTakesTheExit`
/// (a reader that read a proven self-claim whose process is gone as a live
/// supervisor's, the reader before 2026-10-16: the self-supervised agent
/// the act ended was left at a bare shell with nobody to relaunch it —
/// `TheActsEndIsRelaunched`), and `CommitDropsTheCopy` (the crash rule's
/// relaunch before 2026-10-16, owed through its back-off in this instance's
/// memory alone: a Commit meanwhile lost it, and the successor read the end
/// with no line — `TheCopysDecisionRidesTheCommit`), and `ReleaseHandsToLane`
/// (the reader before 2026-10-16's admission flip: a codex-alab's end with no
/// copy — its claim given back, or carried unproven across a Commit — read
/// and relaunched by the Codex lane — `NoCodexEndIsTheLanes`). Tier-1 (`aterm-gui`'s
/// `harness_host` tests
/// `the_real_restart_act_conforms_to_its_model`,
/// `a_second_restart_that_sends_nothing_leaves_the_first_acts_stamp`,
/// `the_restart_acts_kill_waits_out_another_hand`, for `Read`'s
/// `alive == 0`
/// `a_stamped_program_stopped_under_a_hand_that_came_and_went_is_relaunched`,
/// for `ReadByClaim` at a stopped program's end
/// `a_stopped_programs_end_under_another_supervisors_claim_is_that_supervisors`,
/// for `Read` after a hold that landed between the end and the read
/// `an_acts_end_read_under_a_hold_is_relaunched_once_it_lifts`, for
/// `ReadByClaim` after a claim that landed during that carry
/// the same test's `claim` case, for `Read`
/// under a proven self-claim whose process is gone
/// `an_acts_end_under_a_dead_proven_self_claim_is_relaunched`, for the
/// copy's decision across a `Commit`
/// `a_self_claimed_relaunch_owed_at_a_commit_carries_its_validated_line`
/// (`lost` read off the snapshot the real host relaunched), for `Unclaimed`
/// `a_codex_alab_end_with_no_copy_is_nobodys`, for a `Commit`
/// while `P` runs `a_handed_self_claimed_agent_keeps_no_line_its_successor_never_validated`
/// (the successor keeps no line its own copy did not validate, so `P`'s end
/// before a re-proof is `Read`'s; `relaunch::after_exit_as` never takes a
/// line on an exit of its own),
/// and for `esc`'s one watch `many_restarts_of_one_program_run_one_watch`)
/// drives the real `restart` verb and its two halves over a real pty and a
/// real stand-in, the real stamp, a real hold, and the real reader chain.
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

/// The restart ends its program ([`harness_restart_stamp_model`]): from any state in which the
/// stamp names a live `P`, `P` ends (`[]<>(stamped == 0 || alive == 0)`, and an ended `P` never
/// comes back), under weak fairness of the act's own steps (`Term`: its signal follows its stamp
/// in the same request) and of another hand's release (`HandOff`: every hold, lease, turn and
/// claim lifts at last; one that outlasts the stamp lets the program go, which this model has no
/// clock to say), and strong fairness of the escalation (`Escalate`: the timer fires, and the
/// watch looks again every second while another hand holds it off) and of the carrier's finish
/// (`CarrierFinishes`). Neither `Hears`, nor `EndsOtherwise`, nor a Commit is assumed.
/// `TermOnly` alone breaks it.
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
