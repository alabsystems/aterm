// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-GUI supervisor host's per-session machines: the worker lifecycle
//! (at most ONE supervisor on a session at a time, a failing one restarted
//! for ever — badged past a budget, never turned off — a session another
//! supervisor holds left alone until a claim is released), which exits the
//! host hands a worker at all (a name its roster could not read while the
//! agent still holds its tab is none; the upgrade's own restart is carried
//! on as the upgrade's), and the relaunch of an agent that left
//! its tab (a person's exit is theirs; any other is relaunched on a growing
//! back-off or said to a person — never dropped).

use super::Model;

/// Two restored tabs in the host's one retry queue. A's first step waits;
/// B still gets its first step before A can retry, even when A's due time
/// passes during its own call. `Buggy=1` is the former nested retry loop,
/// which ran A's second step before B's first. Tier-1 drives the real host's
/// restored worker with these outcomes and validates each observed action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restored_first_attempt_model() -> Model {
    crate::ty_model! {
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

/// HARNESS WORK IN FLIGHT AT A SEAMLESS UPDATE'S COMMIT (round four of the
/// 2026-09 update robustness work, plan item 7), for one restored agent `A` a
/// cold restore queued ([`harness_restored_first_attempt_model`]'s queue) and
/// one restart `R` the outgoing instance's harness left in flight in a tab
/// whose agent it had already ended (a shell tab no worker looks at).
///
/// `queued` — `A` is still owed by the outgoing instance's queue; `acting` —
/// its step is running (it stays queued meanwhile); `typed` — a step of `A`
/// typed its line and waits for its agent (`wait:resume`): its relaunch is
/// in flight on the conversation's record, and every later step — here or
/// the successor's — carries that on, never typing a second line; `ran` —
/// an agent ran in `A`'s tab since it was queued (the world's fact, a
/// ghost); `overtaken` — the host knows it, of an `A` that typed nothing
/// (round twelve: kept for good once seen); `paused` — a handoff parked the
/// readers; `captured` — what the park's pending layout carried for `A`;
/// `capture` — how a layout reads the queue (0 live, as it is; 1 dropping
/// it, 2 the copy the park froze: the mutants); `committed` — the successor
/// took over (the outgoing instance and any step it was running end there);
/// `succ` — the successor's own queue owes `A`; `resolved` — `A` was
/// relaunched or said; `late` — a step that STARTED while parked; `over` — a
/// relaunch of `A` that could type its line (none typed yet) stepped after
/// an agent ran in its tab; `restart` — `R`'s record is in flight; `swept` —
/// the successor's Commit owes `R` a carry-on step.
///
/// * `StepStart`, `StepRetries`, `StepTypes`, `StepResolves` — the outgoing
///   worker's step and its ends (a word that is not possible yet, typed
///   nothing, keeps `A` queued; one that typed its line and waits keeps it
///   queued, typed); no step STARTS while parked or once an agent is known
///   to have run in the tab of an `A` that typed nothing, and the step in
///   flight ends while parked too.
/// * `AgentRuns` — a person (anyone) starts an agent in `A`'s tab, parked or
///   not, during a step or between steps, and the host's roster names it —
///   which overtakes an `A` that typed nothing, and no `A` that typed: the
///   agent in its tab is then its own (`RestoredAttempt::typed`, an agent
///   seen during the step that typed included); `Overtaken` — the queue
///   takes `A` off without a step (never while parked: its worker waits on
///   the park).
/// * `Park` captures the pending layout from the queue AS IT IS (`A` while
///   queued and not overtaken); `Rollback` resumes the queue.
/// * `Commit` re-captures the layout the same way and goes ahead only where
///   the two captures agree — a step that landed or an agent that ran in
///   between refuses it, as activity — handing what they carry to the
///   successor's queue and owing every restart record still in flight a
///   carry-on.
/// * `SuccRelaunches`, `SweepCarriesOn` — the successor's steps (each the
///   same idempotent step the outgoing instance would have taken, so a
///   relaunch it already typed is carried on, never typed twice).
/// * `RestartFinishes` — the outgoing instance's own act finished `R` before
///   the Commit.
///
/// A TYPED `A` CROSSES A HANDOFF UNTYPED (the review of round twelve): the
/// successor queues what the leaf carried as a fresh attempt, so an agent
/// its roster names in the tab — the one the outgoing instance's line
/// started — overtakes it there, and its queue takes it off with no row.
/// What it typed is the conversation's record in flight, which the
/// successor's Commit sweeps on every adopted tab (`R`'s carry-on, the same
/// step): carried on, or said in the restored agents' row. So the
/// successor's `SuccRelaunches` here is that one step, whichever of the two
/// takes it.
///
/// * `NoSilentLoss` — past the Commit `A` is resolved, the successor owes
///   it, or an agent ran in its tab: the reopened layout's row said it
///   resumes;
/// * `NoStepWhileParked` — the worker takes no new step while the terminal is
///   parked;
/// * `NoRelaunchOverAgent` — no relaunch of `A` is stepped, here or by the
///   successor, once an agent ran in its tab having typed nothing;
/// * `NoStrandedRestart` — past the Commit a restart record in flight is owed
///   a carry-on.
///
/// `Buggy=1` is what shipped, each mutant caught alone: `ParkDropsQueue`
/// (until round four the queue was the worker's local and the handoff leaf
/// never named an agent — `NoSilentLoss`), `StepStartWhileParked` (a worker
/// the park does not stop — `NoStepWhileParked`), `CommitWithoutSweep` (the
/// successor resumed its host for agent tabs only — `NoStrandedRestart`),
/// `ParkFreezes` (rounds four to ten froze the queue at the park for both
/// captures, which then always agreed — `NoRelaunchOverAgent`: the successor
/// typed its relaunch beside the agent a person started there the moment it
/// gave the shell its prompt back), `StepStartOvertaken` (before round eleven
/// the queue never asked the tab before a step — `NoRelaunchOverAgent`) and
/// `AgentStopsForgotten` (round eleven asked whether an agent runs at the
/// instant of a capture or a step, which a ctrl-z or its exit clears —
/// `NoRelaunchOverAgent`). The bounded hold that keeps an AUTOMATIC update
/// from parking while `A` is queued is the ladder's
/// (`native_update_apply_ladder_model`'s `warmup`); this machine is what an
/// explicit update, and one past the hold's bound, relies on. Tier-1
/// (`aterm-gui`'s `harness_host` test
/// `the_real_restored_carry_conforms_to_its_model`) drives the real host's
/// queue through a park mid-step, a step that lands while parked, an agent
/// that runs and stops in the tab, a successor's relaunch of what it carried
/// and the Commit's sweep, and an agent in the tab of a relaunch that typed
/// its line (still stepped, still carried), projecting each observed state
/// onto the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restored_carry_model() -> Model {
    crate::ty_model! {
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

/// A CRASHED AGENT'S RELAUNCH THROUGH A PARK'S COMMIT THAT FAILS (round six's
/// F13, rounds eleven to fourteen). `aterm-gui`'s `harness_host` for ONE
/// session: its agent, its worker, the mark a Commit leaves on the session
/// (`Kept::commit`), the mark of an exit no worker was handed
/// (`Kept::unwatched`), through one Commit that fails and rolls back, then
/// the next park — or a quit.
///
/// `agent` — 1 runs in its tab, 0 crashed (relaunched once read); `worker` —
/// 0 none, 1 supervising (no exit handed), 2 handed the exit (`left`) and
/// not read it yet, 3 read it: the relaunch owed on its back-off; `stop` —
/// that worker's stop is set: the Commit's, or a hand-over's; `handover` —
/// a changed policy handed that worker over to a successor
/// (`Worker::hand_over`); `commit` — 0 `No`, 1 `Stopped`, 2 `Stranded`, 3
/// `TakenUp`; `unwatched` — a worker the Commit stopped, or one handed over,
/// ended handed nothing, or the host read an agent back in the tab of a
/// worker handed its exit (the agent's own worker not started yet); `phase`
/// — 0 before the Commit, 1 the Commit holds the
/// host (suspended: no pass hands an exit), 2 the Commit failed (the host
/// supervises again), 3 rolled back (`relaunch_stranded`), 4 the next park,
/// 5 its Commit went ahead; `done` — a worker relaunched the agent; `queued` — its relaunch is on the
/// restored queue; `stale` — an overtaken cold-restore relaunch of the same
/// tab is still queued (an agent a person started there is the one that
/// crashed); `captured` — what the next park's layout carries for the tab;
/// `succ` — that Commit handed the successor the relaunch; `dropped` — it went
/// ahead with a relaunch queued for the tab that its layout did not carry;
/// `blind` — the exit the host's mark holds is one a worker handed it cannot
/// decide: of an agent a landing started that nothing read, its own start
/// having swept the record the exit before it left and the host's follow
/// having forgotten the snapshot as it held the tab (only `SettleGone`
/// marks one); `closed` — the host shut down (a quit) and its restored
/// queue is closed: nothing more is queued, stepped or handed, and the quit
/// layout carries what the queue and the sessions still owe.
///
/// * `Crash` — the agent crashes, whatever the phase (before a quit), while
///   a worker or the host's mark holds it; `Back` — an agent runs in the tab again (the
///   relaunch's own, or one someone started) and the host reads it: in the
///   tab of a worker handed its exit, and no stop since, it is the host's
///   until its own worker starts (`Worker::back` marks it); `Settle` — that
///   worker reads the agent back, or its relaunch landed (an agent the host
///   read, or one at a Commit's stop, which marks it), and ends — the agent
///   running as it reads it; `Rearm` — it reads the agent back gone again
///   first, in its back-off or at the landing that started it (round two of
///   the joint audit of 2026-10-01): no stop came, so the exit is its own
///   relaunch's again, the mark taken back (a stop's is `Decide`'s).
/// * `Hand` — the host's pass hands the exit to a worker not stopped (never
///   while suspended); `Read` — the worker reads it, stopped or not (round
///   twelve: `left_at_commit`); `Relaunch` — its back-off lands, never while
///   parked (phases 1, 2 and 4); `HandOver` — a changed `[harness]` policy
///   stops a worker whose agent the pass found in the tab, for a successor
///   the next pass starts (`Start`): its stop is set before any Commit's.
/// * `Suspend` — the Commit's stop: every live worker stopped and marked;
///   `Decide` — a stopped worker owing the relaunch decides on the mark,
///   however late (round eleven): `Stopped` leaves it stranded, `TakenUp`
///   queues it — or, the queue closed by a quit, leaves it stranded for the
///   quit layout (round sixteen); `EndUnhanded` — a stopped worker handed
///   nothing ends, leaving the exit to the host: one the Commit stopped
///   (round thirteen), or one
///   handed over (round fourteen), which the Commit's stop does not mark.
/// * `Resume`, `Rollback` — the Commit failed: the host supervises again, then
///   a stranded relaunch is queued and a mark not decided becomes `TakenUp`;
///   `HandUnwatched` — a host supervising (never while suspended) hands an
///   exit no worker was handed to a worker that reads it alone; `Start` — a
///   worker for an agent in the tab (both reset the marks); `QueueSteps` —
///   the queue relaunches it.
/// * `Overtook` — the cold restore's relaunch of the tab, overtaken by the
///   agent a person started there, still waits for its retry; `Park` — the
///   next park captures the queue for the tab; `Commit` — its Commit
///   re-captures it and goes ahead only where the two captures agree (a
///   relaunch queued in between refuses it, as activity).
/// * `Quit` — the host shuts down after the rollback told a worker not
///   decided yet to take its relaunch up (`TakenUp`), closing the queue.
///   The machine's quit is that one: a crash after it, and a worker in an
///   ordinary back-off at it, are the quit layout's to carry from the agent
///   the tab ran (the host's published snapshot), which is not modelled.
///
/// * `NoOwedRelaunchLost` — past the rollback a crashed agent is relaunched,
///   queued, carried, or held by someone who will: a live worker, the
///   rollback's stranded mark or the host's unwatched one. Past the next
///   Commit (phase 5) a worker or a mark still holding it is
///   `HostHandle::pending_restored_for`'s stated residual — a crash read
///   after the Commit's re-capture is on neither layout — not this model's
///   claim, whose next park carries the restored queue alone;
/// * `NoQueuedRelaunchDropped` — no Commit goes ahead past a relaunch the
///   queue owes the tab that its layout does not carry.
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
/// it no more: nothing held it for the quit layout — `NoOwedRelaunchLost`)
/// and `ParkFirstFound` (round twelve's
/// captures read the first relaunch queued for the tab, the overtaken one —
/// ONE reader, so the mutant sets `firstfound` and the Commit's re-capture,
/// the same code, reads that way too: both carried nothing, agreed, and the
/// Commit went ahead — `NoQueuedRelaunchDropped`). One mutant, not a park
/// mutant and a Commit mutant: either capture alone reading first-found
/// disagrees with the other and refuses the Commit, so neither alone is a
/// defect a checker could catch (`ty --strict-vacuity`). Tier-1 (`aterm-gui`'s
/// `harness_host` tests `the_real_commit_owed_relaunch_conforms_to_its_model`,
/// `a_crash_after_a_hand_over_conforms_to_the_commit_owed_model` and
/// `an_agent_back_and_gone_again_conforms_to_the_commit_owed_model`) drives
/// the real host through both losses of round twelve, the control of the
/// first (the agent alive across the failed Commit), the hand-over's loss of
/// round thirteen with a Commit and without, and a relaunch's agent back and
/// gone again — read in the wait, landed, and across a failed Commit — or
/// landed in a Commit's hold and gone there, projecting each observed state
/// onto the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_commit_owed_model() -> Model {
    crate::ty_model! {
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

            action Crash when (
                agent == 1 && (worker > 0 || unwatched == 1) && done == 0 && queued == 0
                    && closed == 0
            ) {
                agent = 0;
            }
            action Back when (agent == 0) {
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
            action Hand when (agent == 0 && worker == 1 && stop == 0 && (phase == 0 || phase > 1)) {
                worker = 2;
            }
            action Read when (worker == 2) {
                worker = 3;
            }
            action StopEndsUnread when (Buggy == 1 && worker == 2 && stop == 1) {
                worker = 0;
            }
            action Relaunch when (worker == 3 && stop == 0 && (phase == 0 || phase == 3)) {
                worker = 0;
                agent = 1;
                done = 1;
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
                worker == 0 && agent == 0 && unwatched == 1 && (phase == 0 || phase > 1)
                    && closed == 0
            ) {
                worker = if (blind == 1) { 0 } else { 2 };
                stop = 0;
                handover = 0;
                commit = 0;
                unwatched = 0;
                blind = 0;
            }
            action Start when (
                worker == 0 && agent == 1 && (phase == 0 || phase > 1) && closed == 0
            ) {
                worker = 1;
                stop = 0;
                handover = 0;
                commit = 0;
                unwatched = 0;
                blind = 0;
            }
            action QueueSteps when (queued == 1 && phase == 3 && closed == 0) {
                queued = 0;
                agent = 1;
                done = 1;
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
                || succ == 1 || worker > 0 || unwatched == 1 || commit == 2;
            invariant NoQueuedRelaunchDropped: dropped == 0;
        }
    }
}

/// `aterm-gui`'s `harness_host` runs one worker per Claude session. For ONE
/// session: `wanted` is the session's published program being `claude` —
/// or, for a pass that cannot name it, its agent still holding the tab
/// ([`harness_leave_model`]) — and the policy active; `cur` a worker running under the current policy;
/// `old` workers asked to stop (the program left, the policy changed) whose
/// wait has not ended yet; `faults` the SESSION's failed runs in the restart
/// window — across its workers, kept at most `Budget + 1`; `faulted` the
/// session badged while its worker waits to restart a supervisor that keeps
/// failing past the budget (the worker still the session's: `cur` stays);
/// `held` another supervisor's claim refused ours.
///
/// * `Arrive`/`Leave` — the program becomes / stops being claude. Leaving asks
///   the worker to stop and forgets `held` (the host keeps it only for a
///   session it still wants) — but NOT `faults`: a program flap gives a
///   crash-looping engine no fresh budget. The badge goes with the worker
///   that raised it, at its `Exit`.
/// * `Start` — the host starts a worker for a wanted session with none of its
///   own, not held, and — the property — NO OLD WORKER STILL RUNNING: the
///   host reaps a stopped worker before it starts the next one. It inherits
///   the session's `faults`.
/// * `Exit` — an old worker's wait ends and it is reaped, its badge cleared
///   on its way out (no new worker starts before it has gone).
/// * `Fail` — the current worker's run failed or panicked: restarted in
///   place, ALWAYS — within `Budget` on the short pauses, past it badged
///   while it waits for the longer one. (Until the philosophy review of
///   2026-09-25 the next failure past the budget turned the supervisor
///   OFF until someone edited `[harness]`: a give-up that waited on a
///   person.)
/// * `Restart` — the failure is told steady: `faulted` is the window's claim that
///   the supervisor keeps failing (it was a tab badge until 2026-09-28), and it
///   is withdrawn when a run has outlived its failure
///   (`HostNote::SupervisorSteady`), not when the next run merely starts — a
///   restart after the pause leaves it told. `Exit` and `Hold` withdraw it too:
///   the worker's end steadies it.
/// * `Age` — the oldest failure leaves the hour's window.
/// * `Hold` — the claim was refused: the worker ends, its badge with it;
///   `Release` — a claim was released or lapsed somewhere, so a held
///   session may be tried again.
/// * `Reload` — the `[harness]` policy changed: the worker is asked to stop
///   (its successor starts under the new policy), and `faults` is forgiven.
///
/// `Buggy=1` is a host without its guards, each one a defect with its own
/// witness: it starts the new worker without waiting for the old one to end
/// (two supervisors answering one session's boxes, the double press the claim
/// exists to prevent — `OneSupervisor`) and over another supervisor's claim
/// (`HeldIsOff`), it counts failures past the window's cap (`FaultBudget`),
/// and it GIVES UP on a supervisor failing past the budget — the worker
/// ended with the badge up, nobody supervising and nothing to restart it
/// (`NeverGivesUp`: a session whose failure the window was told of is
/// still supervised, or its worker is on its way out). The shipped
/// guard on `Fail` is `cur == 1` alone — `faults` never passes `Budget + 1`
/// there — and the `faults <= Budget + 1` half only bounds the buggy host's
/// space. Tier-1 (`aterm-gui`'s `harness_host` conformance)
/// drives the real host through each action and checks its observed state
/// against the model's after the same actions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_worker_lifecycle_model() -> Model {
    crate::ty_model! {
        HarnessWorkerLifecycle {
            const Buggy = 0;
            const Budget = 5;
            var wanted = 0;
            var cur = 0;
            var old = 0;
            var faults = 0;
            var faulted = 0;
            var held = 0;

            action Arrive when (wanted == 0) {
                wanted = 1;
            }
            action Leave when (wanted == 1 && old + cur <= 1) {
                wanted = 0;
                old = old + cur;
                cur = 0;
                held = 0;
            }
            action Start when (wanted == 1 && cur == 0
                && ((held == 0 && old == 0) || Buggy == 1)) {
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

            invariant OneSupervisor: old + cur <= 1;
            invariant FaultBudget: faults <= Budget + 1;
            invariant NeverGivesUp: faulted == 0 || cur == 1 || old > 0;
            invariant HeldIsOff: held == 0 || cur == 0;
        }
    }
}

/// ONE SESSION'S SUPERVISOR CLAIM ACROSS A SEAMLESS UPDATE (the round-four
/// plan of the 2026-09 update robustness work, item 9): an external
/// supervisor that held the session in the outgoing instance is never
/// displaced by the incoming instance's own in-GUI host.
///
/// Before the Commit (`phase` 0, the outgoing instance): the session may be
/// held by an external supervisor's `ttl=` lease (`ext` 1) or by a claim
/// bound to its connection (`ext` 2), or by the outgoing instance's own host
/// (`own`); the outgoing build may be one from before the carry (`older`),
/// which writes no claim facts at all. `Park` draws the handoff record.
/// `Commit` is the incoming instance's adoption as the record allows it:
/// `server` is the claim the incoming instance shows (0 none, 1 the external
/// holder's, 2 its own host's) — a carried lease seeded, nothing else — and
/// `grace` the ticks its host holds the session off, which it does unless
/// the record vouched that nobody but the outgoing host held it
/// (`SessionRecord::claim_grace`).
///
/// After the Commit, time passes (`Tick`) while the grace runs or a live
/// external holder owes its renewal, which reaches the incoming instance
/// within `Renew` ticks (`ExternalRenews`: it renews its lease, or claims
/// again after its connection's claim ended; refused if the host took the
/// session). A carried lease may lapse before that renewal arrives (`Lapse`:
/// a short `ttl=`, or a renewal the outgoing instance took after the park).
/// `HostClaims` is the incoming host's worker claiming a free session once
/// its grace is over; `took` records a claim made before a live external
/// holder's renewal arrived — the takeover.
///
/// * `NoTakeover` — the incoming host never takes a session a live external
///   supervisor held (requires `Grace` to outlast `Renew`: the real grace,
///   `harness_host::ADOPTED_CLAIM_GRACE`, is two renewal steps and a margin);
/// * `NoPhantomClaim` — the incoming instance never shows a claim no external
///   supervisor made;
/// * `NoNeedlessGrace` — a session the record vouched for is supervised at
///   the Commit, never held off.
///
/// `Buggy=1` is three dead Commits, one per claim: `CommitUncarried` — what
/// shipped until round four: no claim carried and no grace, so the host
/// claimed at the Commit and the external supervisor's renewal was refused
/// (`NoTakeover`); `CommitOwnLease` — carrying the outgoing host's own lease,
/// which would park the incoming host behind a dead process's claim
/// (`NoPhantomClaim`); `CommitGraceAll` — a grace on every session, holding
/// off supervision after every update for no one (`NoNeedlessGrace`).
/// Tier-1 (aterm-gui's `session_store` test
/// `the_real_claim_carry_conforms_to_the_handoff_claim_model`) drives the
/// real record projection, TOML wire, seed and grace decision through every
/// pre-Commit configuration and checks the incoming state against `Commit`;
/// `harness_host`'s grace test drives the real host's gate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_handoff_claim_model() -> Model {
    crate::ty_model! {
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

/// THE HOST HANDS A WORKER ITS AGENT'S EXIT ONLY FOR AN EXIT (2026-09-28):
/// `aterm-gui`'s `harness_host`, for ONE supervised session whose tab lives
/// on, under `[harness] relaunch = false` (the configuration of the gate
/// failure below). `alive` the FACT the host reads that the agent holds its
/// tab (`Acts::holds` answering `Some(true)`): the foreground group it was
/// named in is still the tab's, that group's leader still exists, and
/// nothing read for it since could not host it — the hold, not the agent
/// itself (WHAT THE MODEL LEAVES OUT, below); `named` the host's roster
/// names it (its published program, or its reader); `ours`
/// the worker's own upgrade step ended it, and the relaunch that step makes
/// has not landed (the restart in flight at its exit); `stray` the tab has an
/// upgrade restart in flight at its exit that is NOT this agent's — another
/// process's (a person's `claude` at the prompt an earlier relaunch waited
/// on), a relaunch's own record, or one too old to act on; `due` the host
/// owes itself a pass (its bell rang, or a timed look is set); `handed` the
/// host handed the worker the exit (`Worker::leave`); `decided` what the
/// worker made of it: 1 carried on (relaunched), 2 left and said (the
/// owner's limit).
///
/// * `Misread` / `Reread` — the roster stops / starts naming a live agent: a
///   name the resolver could not read replaces the good one
///   (`set_program(None)`) while no reader identifies the agent — the failed
///   name leaves the reader alone, and the roster names the agent by its
///   reader alone when it can (`agent_of`) — or a runtime-named agent
///   (`node`) loses its reader; a later read names it again. Either moves
///   the published name or reader, which rings the host.
/// * `Exit` — the agent ends on its own. Its foreground group moves, which
///   rings the host only while the agent was NAMED
///   (`SessionTimeline::note_foreground_group` rings when a name or a reader
///   goes). An exit during a misread rings nothing HERE, the conservative
///   case: live, a runtime-named agent's name `node` goes with its group (a
///   ring), and a name-failed one's exit rings as the shell's name resolves
///   (`set_program`, from none to the shell's); only when that fails too
///   does nothing ring, and the look `Visit` sets is the backstop. A model
///   green without those rings is no reason to drop them.
/// * `Terminate` — the worker's upgrade step ended the agent (its SIGTERM)
///   and the relaunch it makes waited; the step's end rings the host
///   (`note_upgrade_act`). While the step runs the host takes nothing
///   (`acting`), so the step is one action here.
/// * `Stray` — a restart record in flight for the tab that is not this
///   agent's appears (it rings nothing).
/// * `Visit` — a host pass, which reads the fact (`Acts::holds`) for a worker
///   the roster does not name: an agent that still holds its tab is no exit
///   — the worker is kept, and a short look is set, because nothing else
///   would ring for an exit that follows; one that holds it no longer is
///   handed the exit.
/// * `Decide` — the worker handles the exit it was handed
///   (`on_agent_left`): the upgrade's own restart in flight — asked about
///   THE LEAVING AGENT's pid (`Acts::restarted`, `relaunch::restarted`),
///   read once for every try — is carried, whatever `[harness] relaunch`
///   says; its own exit is left, and said.
///
/// `NoExitWhileAlive`: no worker is handed the exit of an agent that still
/// holds its tab. `NeverMissed`: an exit is handed, or a pass is still owed.
/// `TheUpgradesOwnIsCarried`: an agent the upgrade ended is never left for
/// `[harness] relaunch = false`. `OnlyTheUpgradesOwnIsCarried`: nothing else
/// is carried on against it. The invariants restate `Visit`'s and
/// `Decide`'s own expressions: they are non-vacuous through the `Buggy`
/// hosts below, each caught alone, not by themselves.
///
/// WHAT THE MODEL LEAVES OUT, so its green is not read as more. `alive` is
/// the hold, and every state here with `alive = 1` is one where the agent
/// runs: the model has NO state where the named group's foreground is held
/// by something that is not the agent — a zombie leader not yet reaped, a
/// recycled leader pid behind a foreground group that is gone, a runtime
/// that outlived its agent in the group, an `exec` into an image no name was
/// read for. So `NoExitWhileAlive` is about the host's reading of the hold,
/// not about the agent, and nothing here proves the host never supervises a
/// non-agent: the keep's ceiling (`HOLDS_KEEP_MAX`, 30 s) is what bounds
/// such a keep — and past it the host hands the exit of even a live agent,
/// the behaviour before the keep, which a model with no time does not show
/// (nor a stale hold's `NeverMissed`, which a look owed for ever satisfies).
/// A hold that cannot be read (`None`: a ConPTY has no foreground group)
/// keeps nothing and hands a live agent's exit, the documented trade; it is
/// `alive = 0` here, and `NoExitWhileAlive` claims no more than the host
/// does only under that reading. The worker started again for a kept
/// session, `still_wanted` after a failed run, another group named in a kept
/// agent's stead, the ceiling, and the hold itself are outside the model:
/// `aterm-gui`'s `harness_host` tests bind them
/// (`a_kept_agent_is_looked_at_again_and_nothing_else_is_kept`,
/// `another_group_named_in_a_kept_agents_stead_is_its_exit`,
/// `a_kept_agent_is_followed_at_full_follows_not_at_every_bell`,
/// `a_pass_that_keeps_an_agent_owes_the_host_a_look`, and on a real
/// terminal `the_real_hold_reads_the_tabs_foreground_and_its_leader` and
/// `the_hold_reads_the_groups_leader_and_a_zombie_still_reads_held`).
///
/// `Buggy=1` adds four hosts, each a dead action at `Buggy=0` caught alone:
/// the host of 32a51a716, whose pass hands the exit whenever the roster does
/// not name the agent (`VisitUnconfirmed`: `Misread`, `VisitUnconfirmed` —
/// `NoExitWhileAlive`); a fix of it that keeps the worker but sets no look
/// (`VisitNoLook`: `Misread`, `VisitNoLook`, `Exit` — `NeverMissed`); the
/// worker of 32a51a716, whose exit decision reads `[harness] relaunch` alone
/// (`DecideLimited`: `Terminate`, `Visit`, `DecideLimited` —
/// `TheUpgradesOwnIsCarried`; the gate failure of 32a51a716, where the
/// upgrade's own SIGTERM, both of the step's relaunch attempts waiting — the
/// shape a forced reproduction matched line for line — read as the agent
/// leaving and was dropped); and that fix's first cut, which read the
/// record by tab alone (`DecideByTab`: `Stray`, `Exit`, `Visit`,
/// `DecideByTab` — `OnlyTheUpgradesOwnIsCarried`: a person's crashed agent
/// relaunched against the owner's limit). Tier-1, in `aterm-gui`'s
/// `harness_host` tests: `the_real_host_conforms_to_the_leave_model` drives
/// the real host through every path of the environment's actions and checks
/// after each that what it did — kept, carried on or said — is what the
/// model does. Only `handed` and `decided` are OBSERVED there: `alive`,
/// `ours`, `stray` and `named` are what its driver did to a fake world, `due`
/// is the model's, the hold is the fake's flag (not the shipping `holds`),
/// and its negative controls fire the `Buggy` actions on MODEL state alone.
/// `a_pass_that_keeps_an_agent_owes_the_host_a_look` binds the look `Visit`
/// keeps owed (`due`) to the instant the real host's own pass (`host_pass`)
/// waits for — the one test that catches `VisitNoLook`, since a threaded
/// test host is woken by other tests' bells too.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_leave_model() -> Model {
    crate::ty_model! {
        HarnessLeave {
            const Buggy = 0;
            var alive = 1;
            var named = 1;
            var ours = 0;
            var stray = 0;
            var due = 0;
            var handed = 0;
            var decided = 0;

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
            action Visit when (due == 1 && handed == 0) {
                due = if (named == 0 && alive == 1) { 1 } else { 0 };
                handed = if (named == 0 && alive == 0) { 1 } else { 0 };
            }
            action VisitUnconfirmed when (Buggy == 1 && due == 1 && handed == 0) {
                due = 0;
                handed = if (named == 0) { 1 } else { 0 };
            }
            action VisitNoLook when (Buggy == 1 && due == 1 && handed == 0) {
                due = 0;
                handed = if (named == 0 && alive == 0) { 1 } else { 0 };
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
            invariant NeverMissed: alive == 1 || handed == 1 || due == 1;
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
/// (`Relaunches::pause`, `Steps` the last, ten minutes); `misses` the
/// attempts in a row that did not relaunch, capped at `Badge`; `restarted`
/// the exit being handled was made by a RESTART OF THE HARNESS'S OWN still
/// owed its relaunch (S0 of the in-flight review, 2026-09-27).
///
/// * `Restarted` — the agent left because the harness's own restart ended
///   it (the upgrade's SIGTERM, a Codex `/exit` it typed, the restart in
///   place's): whoever is at the tab, and whatever the owner's `[harness]
///   relaunch` says, the relaunch is due — the exit is that restart's, and
///   what is typed at the prompt still waits on a person or a holder (the
///   relaunch line's own look). No person returning, no limit and no "end
///   of the launch" leaves it: it lands, keeps being tried, or is said.
/// * `Crash` — the agent left with no person asking: a relaunch is due.
/// * `PersonExit` — a person's `/exit`, ctrl-c twice, or a holder's exit:
///   the tab is theirs.
/// * `Limited` — nobody owns the exit (or the pending relaunch), but the
///   owner's `[harness] relaunch = false`, or an agent the relaunch is not
///   written for, takes the power away: nothing due, and said.
/// * `PersonRuns` — the person starts an agent there themselves: nothing
///   is due, and the relaunch's word goes.
/// * `Land` — an attempt relaunched it: running again, the misses and the
///   badge forgotten, the back-off one step longer.
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
/// relaunched and never said.
/// Tier-1 (`the_real_relaunch_decisions_conform_to_the_model`,
/// `aterm-agent`) drives the real `on_exit`, `outcome` and `Relaunches`
/// through every action, and at every state a restart holds asks the real
/// type each exit the guards above refuse it: a person, a limit, and an end
/// its carry could never answer, which the real type says as `Cannot`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_relaunch_on_exit_model() -> Model {
    crate::ty_model! {
        HarnessRelaunchOnExit {
            const Buggy = 0;
            const Steps = 3;
            const Badge = 3;
            var running = 1;
            var asked = 0;
            var pending = 0;
            var badged = 0;
            var step = 0;
            var misses = 0;
            var restarted = 0;

            action Restarted when (running == 1) {
                running = 0;
                pending = 1;
                asked = 0;
                restarted = 1;
            }
            action Crash when (running == 1) {
                running = 0;
                pending = 1;
                asked = 0;
                restarted = 0;
            }
            action PersonExit when (running == 1) {
                running = 0;
                asked = 1;
                pending = if (Buggy == 1) { 1 } else { 0 };
                restarted = 0;
            }
            action Limited when (running == 1 || (pending == 1 && (restarted == 0 || Buggy == 1))) {
                running = 0;
                pending = 0;
                badged = if (Buggy == 1) { badged } else { 1 };
                restarted = if (running == 1) { 0 } else { restarted };
            }
            action PersonRuns when (running == 0 && pending == 0) {
                running = 1;
                asked = 0;
                badged = 0;
                restarted = 0;
            }
            action Land when (pending == 1) {
                running = 1;
                pending = 0;
                badged = 0;
                misses = 0;
                step = if (step <= Steps - 1) { step + 1 } else { Steps };
                restarted = 0;
            }
            action Miss when (pending == 1) {
                misses = if (misses <= Badge - 1) { misses + 1 } else { Badge };
                badged = if (misses + 1 > Badge - 1) { 1 } else { badged };
                step = if (step <= Steps - 1) { step + 1 } else { Steps };
            }
            action Ended when (pending == 1 && (restarted == 0 || Buggy == 1)) {
                pending = 0;
                asked = 1;
            }
            action Cannot when (pending == 1) {
                pending = 0;
                badged = if (Buggy == 1) { badged } else { 1 };
            }
            action Healthy when (running == 1 && step > 0) {
                step = 0;
            }
            action PersonReturns when (pending == 1 && (restarted == 0 || Buggy == 1)) {
                pending = 0;
                asked = 1;
            }

            invariant PersonWins: asked == 0 || pending == 0;
            invariant NeverSilent: running == 1 || pending == 1 || badged == 1 || asked == 1;
            invariant RestartCarried: restarted == 0 || asked == 0;
        }
    }
}

/// WHAT AN EXIT LEFT, READ AS IT IS SEEN (D2 of the 2026-09-26 live test):
/// the relaunch on exit tells a crash from a graceful exit by Claude Code's
/// own record of the agent (`sessions/<pid>.json`) — a crash cannot remove
/// it, a graceful exit (a person's or an orchestrator's `/exit`, a `kill`)
/// does — and that record has a SECOND writer: any Claude Code that starts
/// removes dead processes' records (measured on 2.1.283: a SIGKILLed agent's
/// record gone 0.79 s after the kill, as a Claude in another tab started).
/// For ONE exit of ONE agent: `exit` 0 it runs, 1 it crashed, 2 it exited
/// gracefully; `record` its record is on disk; `seen` the host's look at the
/// exit (`aterm-agent`'s `relaunch::exit_record`) has decided; `kept` what
/// that look found (1: the record survived); `decided` 0 no attempt yet, 1
/// relaunched, 2 left as a graceful exit.
///
/// * `Crash` / `Graceful` — the agent ends. A graceful exit's record may
///   still be on disk as the exit is seen (it removes it in its last
///   moments: measured gone 0.02 s after the `/exit`).
/// * `OwnRemoval` — the graceful exit removes its own record.
/// * `Look` — the host's look decides, and its answer is KEPT: at once on a
///   removed record, else once `EXIT_SETTLE` (a quarter second) has passed,
///   which outlasts an exit's own removal — so a graceful exit is decided
///   only once its removal has landed.
/// * `Sweep` — another Claude Code starts and removes the dead agent's
///   record: at any time after the look, the relaunch's whole back-off (1 s
///   to ten minutes) included. (One inside the quarter second of the settle
///   is the window this design leaves: the assumption the guard states.)
/// * `Decide` — an attempt (`relaunch::after_exit`), after the back-off
///   (which outlasts an exit's own removal too): relaunched iff the KEPT
///   look found the record.
///
/// `CrashIsRelaunched`: a crash is never left as a graceful exit.
/// `GracefulIsLeft`: a graceful exit is never relaunched.
///
/// `Buggy=1` adds two hosts, each a dead action at `Buggy=0` caught alone:
/// the host of 57a2b7050, whose attempt reads the record as it stands then
/// (`DecideAtAttempt`: `Crash`, `Look`, `Sweep`, `DecideAtAttempt` leaves a
/// crash — `CrashIsRelaunched`), and the naive fix of it, whose look reads
/// at the detection instant, before a graceful exit's own removal
/// (`LookAtInstant`: `Graceful`, `LookAtInstant`, `OwnRemoval`, `Decide`
/// relaunches it — `GracefulIsLeft`). Tier-1
/// (`the_real_exit_read_conforms_to_the_model`, `aterm-agent`'s upgrade
/// tests) replays every path of the model through the real `exit_record`,
/// `look_at_exit` and `after_exit` over a real record of a dead process;
/// the first witness through the real read at the attempt
/// (`ExitRecord::Unread`); and an own removal landing inside the real
/// look's settle, which the real look reads as `OwnRemoval` then `Look` —
/// never as `LookAtInstant`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_exit_record_model() -> Model {
    crate::ty_model! {
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

/// A CODEX EXIT, READ ON ITS SHELL'S WORD (aterm-agent
/// `upgrade_codex_drive.rs` `look_at_exit`, `relaunch::exit_record` and the
/// Codex lane's `after_exit`: the Codex parity of 2026-09-27 and the review
/// of that day). Codex keeps no record a crash would leave behind: the
/// witness is the shell's exit status for the command that ran it — `0` its
/// own `/exit`, 129/130/143 someone's SIGHUP/SIGINT/SIGTERM, anything else a
/// crash — drawn as the shell takes the terminal back, and looked for up to
/// `EXIT_GONE`. An embedded TUI's thread lock is NO witness: `/exit` removes
/// it, and SIGTERM, SIGINT and SIGHUP leave it exactly as SIGKILL does
/// (measured 2026-09-27 on 0.157.1, `--no-daemon`).
///
/// For ONE exit of ONE embedded Codex: `exit` 0 it runs, 1 it crashed, 2 its
/// own `/exit`, 3 someone's signal; `lock` its thread lock is on disk;
/// `word` the shell has drawn the status; `later` a command run at the
/// prompt since has finished, its block the newest (a crash's status, the
/// worst a read of the newest block could take for the TUI's); `seen` the
/// look has decided and `kept` what it found (1 a crash, 2 no crash, 3 no
/// word within its wait); `decided` 0 no attempt yet, 1 relaunched, 2 left;
/// `readable` the TUI's own word could be read when it decided.
///
/// Every read is of the block the TUI RAN IN (its snapshot's `block`):
/// `Look` reads its word while it is the newest finished block, and once a
/// later command's has taken its place (`blocks 2` holds the last two) it
/// reads none. The attempt (`Decide`) reads that word again where the look
/// found none (round three of the joint audit of 2026-10-01: the word comes
/// through the window's parser, which a seamless update's park stops, and
/// a prompt can come back later than the look waits).
///
/// `TheirsIsLeft`: an own exit or someone's signal is never relaunched.
/// `AToldCrashIsRelaunched`: a crash is left only when its word could not be
/// read at the attempt either.
///
/// `Buggy = 1` adds two looks and two attempts, each dead at `Buggy = 0`
/// and caught alone: the first cut's look, which read the lock whenever the
/// shell had not drawn its word yet (`LookAtLock`: `Signal`, `LookAtLock`,
/// `Decide` relaunches a person's `kill` — `TheirsIsLeft`); Claude Code's
/// rule inherited, silence read at once as a graceful exit (`LookAsClaude`:
/// `Crash`, `LookAsClaude`, `Decide` leaves a crash —
/// `AToldCrashIsRelaunched`); the attempt that decided on the look alone
/// until round three (`DecideOnLook`: `Crash`, `LookGone`, `ShellWord`,
/// `DecideOnLook` leaves a crash whose word was there to read —
/// `AToldCrashIsRelaunched`); and an attempt that reads the newest block
/// whatever ran it (`DecideOnNewest`: `Signal`, `LookGone`, `ShellWord`,
/// `Later`, `DecideOnNewest` relaunches a person's `kill` on another
/// command's failure — `TheirsIsLeft`). Tier-1 (aterm-agent
/// `harness/upgrade_codex_drive_tests.rs`,
/// `tier1_the_real_codex_exit_read_conforms_to_the_model`) replays every
/// look and every attempt of the model through the real `look_at_exit`,
/// `exit_record` and `after_exit` over a stand-in tab, the lock on disk and
/// the blocks drawn as the model has them.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_codex_exit_witness_model() -> Model {
    crate::ty_model! {
        HarnessCodexExitWitness {
            const Buggy = 0;
            var exit = 0;
            var lock = 1;
            var word = 0;
            var later = 0;
            var seen = 0;
            var kept = 0;
            var decided = 0;
            var readable = 0;

            action Crash when (exit == 0) {
                exit = 1;
            }
            action OwnExit when (exit == 0) {
                exit = 2;
                lock = 0;
            }
            action Signal when (exit == 0) {
                exit = 3;
            }
            action ShellWord when (exit > 0 && word == 0) {
                word = 1;
            }
            action Later when (word == 1 && later == 0) {
                later = 1;
            }
            action Look when (exit > 0 && seen == 0 && word == 1 && later == 0) {
                seen = 1;
                kept = if exit == 1 { 1 } else { 2 };
            }
            action LookGone when (exit > 0 && seen == 0 && (word == 0 || later == 1)) {
                seen = 1;
                kept = 3;
            }
            action LookAtLock when (Buggy == 1 && exit > 0 && seen == 0 && word == 0) {
                seen = 1;
                kept = if lock == 1 { 1 } else { 2 };
            }
            action LookAsClaude when (Buggy == 1 && exit > 0 && seen == 0 && word == 0) {
                seen = 1;
                kept = 2;
            }
            action Decide when (seen == 1 && decided == 0) {
                readable = if (kept <= 2 || (word == 1 && later == 0)) { 1 } else { 0 };
                decided = if (kept == 1 || (kept == 3 && word == 1 && later == 0 && exit == 1)) {
                    1
                } else {
                    2
                };
            }
            action DecideOnLook when (Buggy == 1 && seen == 1 && decided == 0) {
                readable = if (kept <= 2 || (word == 1 && later == 0)) { 1 } else { 0 };
                decided = if kept == 1 { 1 } else { 2 };
            }
            action DecideOnNewest when (Buggy == 1 && seen == 1 && decided == 0 && kept == 3) {
                readable = if (word == 1 && later == 0) { 1 } else { 0 };
                decided = if (later == 1 || (word == 1 && exit == 1)) { 1 } else { 2 };
            }

            invariant TheirsIsLeft: exit <= 1 || decided == 0 || decided == 2;
            invariant AToldCrashIsRelaunched:
                exit == 0 || exit > 1 || decided <= 1 || readable == 0;
        }
    }
}

/// THE UPGRADE'S LOOKS AT ONE SESSION, as its worker takes them at the
/// loop's idle points (`aterm-gui`'s `harness_host::WorkerIdle::at_idle`,
/// over `aterm-agent`'s `upgrade_drive::due`), and ITS NOTE BEHIND
/// (`harness_host::ask_note`, over `upgrade_drive::note_behind`), from the
/// worker's attach at a fresh launch. For ONE session: `due` it has an
/// upgrade to take; `readable` it can be read (the instance's socket
/// answers, Claude Code's record of the agent is written — at a launch, a
/// moment after the attach); `ready` it was announced to and answered
/// READY, so its next step restarts it; `looks` a look is owed — the loop
/// parked for the session's next idle point, or a later look set on the
/// ladder; `done` the upgrade said its last word; `owed` its note behind is
/// still to be taken; `noted` its upgrade state is minted, and `aged` with
/// its age from the attach.
///
/// * `Look` — a look that reads an upgrade to take asks the owed note first,
///   then steps it: the notice (the READY answer folded in), then the
///   restart and the last word. A step that finds no state mints one, its
///   age from the step.
/// * `NotDue` — a look that reads nothing to take lets the upgrade go (the
///   note, read, has nothing to note).
/// * `Unread` — a look that could not read says so (`wait:no-socket`,
///   `wait:no-record`) and sets a later look on the ladder.
/// * `Note` / `NoteUnread` — the owed note asked (at the attach, at the
///   host's wakes and ladder, at an idle point): read, it mints the state
///   with the attach's age, or finds nothing to note; unread, it stays owed.
/// * `Lose` / `Regain` — the reads fail and come back (the lanes held and
///   given back; the record written).
/// * `Settle` — the upgrade stops being due by another hand (the owner's
///   `--skip`, a person's own restart onto the build).
///
/// `NeverDropped`: an upgrade due and not done always has a look owed — one
/// that answered READY included (the live re-test of 155c72a28, 2026-09-26:
/// every control lane held, the looks' `who` was refused, `due` read "not
/// due", and four of seven sessions were let go for good — one that had
/// answered READY to "aterm will restart this Claude Code" among them).
/// `BehindFromTheAttach`: a state minted is behind from the attach (the same
/// re-test's N3: the note was asked once, at the attach, before the launch's
/// record was written, and the first step minted the state after the first
/// busy turn, its age from then).
///
/// `Buggy=1` is the host of 155c72a28: an unreadable look owes nothing
/// (`Regain`, `Look`, `Lose`, `Unread`: a READY'd session let go), and an
/// unreadable note is given up (`NoteUnread`, `Regain`, `Look`). Tier-1, in
/// two halves: `aterm-gui`'s `the_real_look_conforms_to_the_upgrade_look_model`
/// drives the real `attach`, `WorkerIdle` and `ask_note` through every
/// transition of the model from every reachable state, and projects the
/// worker's park, later look and owed note onto `looks` and `owed`; and
/// `aterm-agent`'s `the_real_classifier_conforms_to_the_upgrade_look_model`
/// drives the real `upgrade_drive::due_among` and `no_record` over every
/// combination of what a look reads of a tab (the record, its group, its
/// process's ids and argv, its launch's age), projecting each onto `due` and
/// `readable` — `readable` is every read the verdict needed — and checks the
/// model enables the action its verdict is.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_look_model() -> Model {
    crate::ty_model! {
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

/// AN AGE BELONGS TO ONE PROCESS (the owner's tab of 2026-09-28, s-3e79…): a
/// `Pending(to=2.1.283)` record minted 2026-09-25 for the 2.1.274 process
/// outlived the owner's own restart onto 2.1.283, and when 2.1.284 came the
/// retarget carried its three-day-old age onto a process that had run the
/// newest build for a day — "behind for 2 days", a warn row, 11 s after the
/// build appeared. For ONE conversation: `proc` the process that holds it
/// (0 the one the record was minted behind, 1 after the owner's restart by
/// hand); `rec` the record (0 none, 1 counts process 0's age, 2 process 1's);
/// `behind` the holder runs a build older than the newest one; `aged` the
/// record's age is past the judging bound (`STALLED_AFTER_S` today); `alarm` a
/// judgment by that age was made (the row, the tab's mark — whatever reads it).
/// The session starts behind, its record minted for process 0.
///
/// * `Time` — the bound passes on the record.
/// * `HandRestart` — the owner restarts the agent by hand onto the newest
///   build: a new process, no longer behind; the record is the old process's.
/// * `Visit` — the next step finds the session with nothing to move
///   (`current`): a record for a build it now runs is closed
///   (`forget_settled_pending`).
/// * `NewBuild` — a newer build lands: the holder is behind, and the record —
///   or, none standing, the one the next step mints — counts an age carried
///   only while it counts THIS process (`St::for_target`).
/// * `Judge` — something reads the age of a behind session's record, and
///   raises an alarm when it is past the bound.
///
/// `AgeIsThisProcess`: no judgment by an age that is not the holder's.
/// `Buggy=1` is the build that shipped: the record survives the hand
/// restart (`Visit` closes nothing), and the retarget carries whatever age
/// stands. Tier-1: `aterm-agent`'s `the_real_age_conforms_to_the_age_model`
/// drives the real `St::for_target`, `forget_settled_pending` and the owner's
/// view over every reachable state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_age_model() -> Model {
    crate::ty_model! {
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

/// THE EXPLICIT RESTART (design of 2026-10-08, `docs/HANDOFF-robustness-rounds-2026-10-08.md`
/// §4): `aterm ctl @<sid> restart [pid=<n>] [if-stalled]` — "restart this
/// agent in place" — writes ITS RECORD BEFORE ITS SIGNAL, as every other
/// in-place restart of the harness does (the live upgrade, the memory, model
/// and account restarts), and the exit is recognised by LOOKUP, never by
/// inference. Until then the stall's remedy was the one restart whose record
/// came after its exit (round fourteen's `RemedySent`, kept in the instance's
/// memory and spent by a new stall episode), and four review rounds each found
/// an interleaving where the inference was wrong.
///
/// For ONE process `P` holding one tab, and ANY NUMBER OF ACTS on it, one at
/// a time (`restart_act::ACTING`): `alive` — `P` runs; `stamped` — a stamp
/// naming `P` (pid and kernel start) is on disk (`<harness
/// state>/upgrade/asked-<tab>.json`, written atomically); `inflight` — an act
/// has stamped and not yet signalled or given up; `fresh` — that act WROTE
/// the stamp (none naming `P` stood when it read; else it joined the one that
/// did); `termed` — an act's `SIGTERM` was sent; `esc` — how many escalation
/// watches run on `P` in the instance that made the act: one at most, an act
/// that finds one running giving it its stamp's time instead of starting its
/// own (`restart_act::watch`, ruling 19 of 2026-10-09); `committed` — a seamless
/// update's Commit handed the session to a successor (the outgoing
/// instance's threads, an act's request among them, end with it; the stamp,
/// on disk, does not); `ended` — who ended `P` (0 nobody yet, 1 an act: its
/// `TERM`, its `KILL` or the carrier's, 2 anyone else: a person's `/exit`, a
/// crash, a shell's `kill`); `at_end` — the stamp stood when `P` ended;
/// `read` — the exit's reader (the worker that reads it, the exit-only
/// worker, a successor's after a Commit): 0 not yet, 1 relaunched as the
/// act's (`ExitCause::Asked`/`Stall`, a person's keystroke before it not read
/// as their `/exit`), 2 read by the other rules (a graceful exit is left);
/// `hand` — ANOTHER HAND holds the session (a hold, another driver's lease or
/// named turn, another supervisor's claim: what the exit's reader leaves to
/// its holder, `restart_act::another_hand`); `held_kill` — an act's `KILL`
/// was sent while another hand held the session (review of 1a733ed60).
///
/// * `Stamp` — an act's first half (`restart_act::begin`), while `P` leads
///   and no other hand holds the session (its fences): the stamp written —
///   or, one naming `P` standing, JOINED (`fresh` 0).
/// * `Term` — its second half: the discard and `killpg(SIGTERM)`, while `P`
///   still leads (step 1d's re-read), and the one watch on `P` started — or,
///   one running (a joined act's), kept. `SendsNothing` — the re-read found `P`
///   gone (or `killpg` refused): nothing sent, and the stamp the act WROTE
///   taken back (`restart_act::take_back`); a joined one is kept.
/// * `Hears` — `P` runs its `SIGTERM` handler and exits; a deaf `P` (its
///   JS thread spinning, `trap '' TERM`) never does.
/// * `Escalate` — an act's thread, `P` still leading after the grace and no
///   other hand on the session (re-read right before the discard and the
///   `killpg`, ruling 11): `killpg(SIGKILL)`, its `held_kill` the hand it was
///   sent under. `EscalationSeesExit` — its wait on `NOTE_EXIT` ends.
/// * `EndsOtherwise` — anyone ends `P`, at any point.
/// * `HandOn` / `HandOff` — another hand lands on the session, or leaves
///   it, at any point (a fleet halt, the bridge-lost auto-hold, the owner's
///   local hold, a driver's lease or named turn, `drive watch`'s claim).
/// * `Commit` — at any point; `CarrierFinishes` — the successor's carrier
///   finds a stamp naming a live leader past the act's 30 s and finishes the
///   `KILL` (it holds the master now), no other hand on the session (its
///   `held_kill` as `Escalate`'s).
/// * `Read` — the exit is read, by whichever reader, and the stamp it finds
///   naming `P` is consumed: once `P` has ENDED — an exit whose program still
///   runs (stopped away from its tab, its shell in front) is nobody's yet,
///   decided at its end as an exit's first read is, the stamp looked up and
///   another supervisor's claim asked again there (rulings 15 and 17) — and
///   with no other hand on the session, its guard says: an exit read while
///   another hand holds it is that holder's, the reader's documented rule
///   (`relaunch::on_exit`'s `Held`), and not this model's.
///
/// Invariants: `TheActsEndIsRelaunched` — an end an act made is relaunched
/// whoever reads it, however many acts there were; `OnlyAStampedEndIsTheActs`
/// — nothing is relaunched as the act's unless the stamp stood at its end;
/// `TheActEndsItsProgram` — a `P` an act signalled is never left alive with
/// nobody to end it (its thread, or after a Commit the carrier);
/// `TheKillWaitsOutAnotherHand` — no act's `KILL` lands while another hand
/// holds the session (the halt contract; under the act's old `KILL` the
/// reader then left the agent the act had killed at a bare shell);
/// `OneWatchPerProcess` — however many acts there were, one watch ends `P`
/// (bug hunt 7: every joined act's thread held a kqueue of its own, a
/// descriptor per request against the window's 256). Liveness
/// ([`harness_restart_stamp_liveness`]): a stamped `P` ends, once every
/// other hand is released.
///
/// NOT MODELLED, said: the stamp's clock. The act's watch lets a program go
/// once its stamp no longer stands (`STALE_S`: stopped away from its tab,
/// ruling 8, or held by another hand that outlasts it, ruling 11), and no
/// `KILL` — the watch's or a carrier's — is ever sent for a stamp that no
/// longer stands (ruling 12), nor in its last half minute, when its exit
/// would be read after it went stale (ruling 16); an act that JOINS a stamp
/// renews its time once its own `TERM` is sent, so its `KILL` has the
/// stamp's whole life (ruling 18); and an end that comes after the stamp
/// went stale is the program's own, its stamp looked up again at the end
/// (ruling 17). Saying it needs a stale flag threaded through every end, a
/// relaunch invariant weakened to admit the documented heard-after-stale
/// residual, and a record of a `KILL` apart from the program's own end: a
/// rework, not an action; the Tier-1s
/// `a_stamped_program_stopped_inside_its_grace_is_ended_once_it_leads_again`,
/// `a_stamp_that_no_longer_stands_is_no_acts_to_finish`,
/// `a_restart_that_joins_a_stamp_near_its_end_renews_it_and_ends_its_program`
/// and `a_stopped_programs_end_after_its_stamp_went_stale_is_its_own` pin it
/// on the real code. Nor a STOPPED `P` as a state of its own: `alive` covers it,
/// and `Read`'s `alive == 0` is what the reader must wait for (ruling 15).
///
/// `Buggy=1` adds eight mutants, each caught alone: `SignalBeforeStamp`
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
/// `TheActsEndIsRelaunched`) and `WatchPerAct` (an act whose `SIGTERM` is
/// sent starts a watch of its own beside the one running — the act before
/// bug hunt 7 — `OneWatchPerProcess`). Tier-1 (`aterm-gui`'s `harness_host` tests
/// `the_real_restart_act_conforms_to_its_model`,
/// `a_second_restart_that_sends_nothing_leaves_the_first_acts_stamp`,
/// `the_restart_acts_kill_waits_out_another_hand`, for `Read`'s
/// `alive == 0`
/// `a_stamped_program_read_as_gone_while_stopped_is_relaunched_once_the_act_ends_it`,
/// and for its `hand == 0` at a stopped program's end
/// `a_stopped_programs_end_under_another_supervisors_claim_is_that_supervisors`,
/// and for `esc`'s one watch `many_restarts_of_one_program_run_one_watch`)
/// drives the real `restart` verb and its two halves over a real pty and a
/// real stand-in, the real stamp, a real hold, and the real reader chain.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restart_stamp_model() -> Model {
    crate::ty_model! {
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

            action Stamp when (alive == 1 && inflight == 0 && committed == 0 && hand == 0) {
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
            action HandOff when (hand == 1) {
                hand = 0;
            }
            action Commit when (committed == 0) {
                committed = 1;
                esc = 0;
                inflight = 0;
                fresh = 0;
            }
            action CarrierFinishes when (
                committed == 1 && stamped == 1 && alive == 1 && esc == 0 && hand == 0
            ) {
                alive = 0;
                ended = 1;
                at_end = 1;
                held_kill = hand;
            }
            action Read when (alive == 0 && read == 0 && hand == 0) {
                read = if (stamped == 1) { 1 } else { 2 };
                stamped = 0;
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

            invariant TheActsEndIsRelaunched: read <= 1 || ended == 0 || ended == 2;
            invariant OnlyAStampedEndIsTheActs: read == 0 || read == 2 || at_end == 1;
            invariant TheActEndsItsProgram: alive == 0 || termed == 0 || esc > 0
                || committed == 1;
            invariant TheKillWaitsOutAnotherHand: held_kill == 0;
            invariant OneWatchPerProcess: esc <= 1;
        }
    }
}

/// THE RESTART ENDS ITS PROGRAM ([`harness_restart_stamp_model`]): from any
/// state in which the stamp names a live `P`, `P` ends — `[]<>(stamped == 0 ||
/// alive == 0)`, and `P` never comes back once it ended — under the weak
/// fairness of the act's own steps (`Term`: its signal follows its stamp in
/// the same request) and of another hand's release (`HandOff`: every hold,
/// lease, turn and claim is lifted at last — one that outlasts the stamp
/// lets the program go, ruling 11, which this model has no clock to say),
/// and the STRONG fairness of the escalation (`Escalate`: the thread's timer
/// fires, and its watch looks again every second while another hand holds
/// it off) and of the carrier's finish (`CarrierFinishes`: a successor whose
/// Commit cut the act inside its grace finishes the `KILL`, waiting a hand
/// out the same way). Neither the program's handler (`Hears`), nor a person
/// (`EndsOtherwise`), nor a Commit is assumed. The mutant that breaks it
/// alone: `TermOnly`, the act with no escalation, whose deaf program
/// survives.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restart_stamp_liveness() -> super::Liveness {
    use super::{eq, int, or_, var};
    super::Liveness {
        name: "TheRestartEndsItsProgram",
        goal: or_(eq(var("stamped"), int(0)), eq(var("alive"), int(0))),
        weak: vec!["Term", "HandOff"],
        strong: vec!["Escalate", "CarrierFinishes"],
        mutants: vec!["TermOnly"],
    }
}
