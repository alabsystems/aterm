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

use aterm_spec::derive::Model;

/// Two restored tabs in the host's one retry queue. A's first step waits;
/// B still gets its first step before A can retry, even when A's due time
/// passes during its own call. `Buggy=1` is the former nested retry loop,
/// which ran A's second step before B's first. Tier-1 drives the real host's
/// restored worker with these outcomes and validates each observed action.
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
/// 5 its Commit went ahead; `done` — a worker relaunched the agent since its
/// last crash; `queued` — its relaunch is on the
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
/// layout carries what the queue and the sessions still owe; `down` — the
/// host thread panicked and waits out its restart's back-off: no pass hands
/// an exit, a mark or starts a worker (round seventeen); `seen` — what the
/// agent's last exit left was read as it was seen: by the worker a pass
/// handed it at once, or by the exit watch where no pass would see it soon
/// (`Kept::exit_watch`, round seventeen); `swept` — a Claude Code started
/// since that exit and removed the crashed agent's session record, as every
/// Claude Code that starts removes dead agents' records (D2 of the
/// 2026-09-26 live test); `landed` — the agent a worker's relaunch landed,
/// that no pass has named yet, is watched by the exit watch that hands its
/// exit itself once read (`ExitWatched::hands`; round seventeen, the review
/// of its first cut); `qlanded` — the restored queue's step landed an agent
/// no read has reached yet (round eighteen, the review of its first cut):
/// its attempt stays queued, the exit's holder, until a read reaches the
/// agent (`QueueReadsLanded`: watched as `landed`), a pass names it
/// (`Start`), or it crashes unread — its own relaunch's exit again, the
/// attempt owed as before (`Crash`), stepped again.
///
/// * `Crash` — the agent crashes, whatever the phase (before a quit), while
///   a worker, the host's mark or a landing's watch holds it — the agent a
///   relaunch brought back too, which owes a relaunch again — and what it
///   left is read as it is seen; `Sweep` — a Claude Code starts and removes
///   the crashed agent's record; `Back` — an agent runs in the tab again (the
///   relaunch's own, or one someone started) and the host reads it — a pass,
///   so never while the host is down: in the tab of a worker handed its
///   exit, and no stop since, it is the host's until its own worker starts
///   (`Worker::back` marks it); `Settle` — that
///   worker reads the agent back, or its relaunch landed (an agent the host
///   read, or one at a Commit's stop, which marks it), and ends — the agent
///   running as it reads it; `Rearm` — it reads the agent back gone again
///   first, in its back-off or at the landing that started it (round two of
///   the joint audit of 2026-10-01): no stop came, so the exit is its own
///   relaunch's again, the mark taken back (a stop's is `Decide`'s).
/// * `Hand` — the host's pass hands the exit to a worker not stopped (never
///   while suspended, nor while the host is down); `Read` — the worker reads
///   it, stopped or not (round twelve: `left_at_commit`): on what the exit
///   left as it was seen, or — where nothing read it so — on the record now,
///   which a sweep read as a graceful exit (the worker ends, nothing owed);
///   `Relaunch` — its back-off lands, never while
///   parked (phases 1, 2 and 4), and the worker ends, the agent it started
///   left to the roster and watched until a pass names it (`landed`);
///   `RelaunchUnread` — its back-off lands, but no read reaches the agent
///   it started (round eighteen): the worker reads it again while it holds
///   its tab and no pass names it (`harness_host::read_landed`), so it stays
///   the exit's holder — a later read is `Relaunch`'s landing, and the
///   agent's crash meanwhile is its own relaunch's again;
///   `HandOver` — a changed `[harness]` policy
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
///   `HandUnwatched` — a host supervising (never while suspended, nor while
///   it is down) hands an exit no worker was handed to a worker that reads it
///   alone: the host's mark's, or a landing's watch's once it has read the
///   exit; `Start` — a worker for an agent in the tab (both reset the
///   marks), and ends a queued attempt whose unread landing it names;
///   `QueueSteps` — the queue relaunches it, the agent it landed read and
///   watched until a pass names it (`landed`; round eighteen,
///   `harness_host::restored_landing_look`); `QueueStepsUnread` — the
///   queue's step lands an agent no read reaches (`qlanded`), the attempt
///   kept; `QueueReadsLanded` — a later look of the queue reads it (never
///   while a park pauses the queue): the attempt over, the agent watched
///   (`landed`). None waits for the host: the queue runs on its own
///   thread, the host down or not.
/// * `Panic` — the host thread's loop panics (round seventeen): what it held
///   lives on in the host thread's own memory — its workers run on, each its
///   tab's supervisor still, held to the policy as it is (a changed one hands
///   them over: `HandOver`), and each one's agent's exit is watched, as is an
///   agent a carried worker's relaunch lands — and the loop starts again
///   after its back-off, or once news cuts it short (the policy changed, a
///   Commit failed), `Up`, its first pass reading the roster again. A panic
///   in the restart itself is one more panic in a row, caught as the run's
///   is (round eighteen, `harness_host::restart_host`): nothing changes
///   here, `down` stays.
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
///   rollback's stranded mark, the host's unwatched one or a landing's
///   watch. Past the next
///   Commit (phase 5) a worker or a mark still holding it is not this
///   model's claim, whose next park carries the restored queue alone: a
///   crash read after the Commit's re-capture is on neither layout as owed,
///   and since round eighteen the successor takes it by watching the agent
///   the leaf names supervised (`HostHandle::watch_handed`; pinned by
///   `aterm-gui`'s App-level tests, not expressed here);
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
/// `an_agent_back_and_gone_again_conforms_to_the_commit_owed_model` and
/// `exit_watch::an_exit_nobody_is_handed_conforms_to_the_commit_owed_model`)
/// drives the real host through both losses of round twelve, the control of the
/// first (the agent alive across the failed Commit), the hand-over's loss of
/// round thirteen with a Commit and without, and a relaunch's agent back and
/// gone again — read in the wait, landed, and across a failed Commit — or
/// landed in a Commit's hold and gone there, a crash swept while the host
/// is down after a panic or while a Commit holds it (round seventeen), and an
/// agent a relaunch landed while the host is down that crashes before it is
/// back (the review of round seventeen's first cut), projecting each
/// observed state onto the model's; and (round eighteen,
/// `exit_watch::the_landings_of_round_eighteen_conform_to_the_commit_owed_model`)
/// the rollback queue's landing whose agent crashes before a pass names
/// it, a panic in the host's restart, a landing no read reaches whose
/// agent crashes while the host is down, and the rollback queue's landing
/// no read reaches, whose agent crashes unread and is stepped again.
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

/// `aterm-gui`'s `harness_host` runs one worker per agent session. For ONE
/// session: `wanted` is the session's published program being a supervisable
/// agent — or, for a pass that cannot name it, its agent still holding the
/// tab ([`harness_leave_model`]) — and the policy active; `on` that agent's
/// own switch (`[harness.<id>] enabled`); `cur` a worker running under the
/// current policy; `old` workers asked to stop (the program left, the policy
/// changed, the agent switched off) whose wait has not ended yet; `faults`
/// the SESSION's failed runs in the restart window — across its workers,
/// kept at most `Budget + 1`; `faulted` the session badged while its worker
/// waits to restart a supervisor that keeps failing past the budget (the
/// worker still the session's: `cur` stays); `held` another supervisor's
/// claim refused ours.
///
/// * `Arrive`/`Leave` — the program becomes / stops being the agent. Leaving
///   asks the worker to stop and forgets `held` (the host keeps it only for a
///   session it still wants) — but NOT `faults`: a program flap gives a
///   crash-looping engine no fresh budget. The badge goes with the worker
///   that raised it, at its `Exit`.
/// * `SwitchOff`/`SwitchOn` — the agent's switch goes off / on. Off asks the
///   worker to stop exactly as `Leave` does — `faults` kept, `held`
///   forgotten, the badge going at its `Exit` — while the session stays the
///   agent's: the switch-off path is a stop, never the agent leaving
///   ([`harness_leave_model`] hands its worker no exit). On lets `Start` run
///   again.
/// * `Start` — the host starts a worker for a wanted session whose agent is
///   on, with none of its own, not held, and — the property — NO OLD WORKER
///   STILL RUNNING: the host reaps a stopped worker before it starts the next
///   one. It inherits the session's `faults`.
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
/// AUTOPILOT OFF (`docs/DESIGN-harness-autopilot-2026-10-15.md` §2.2; `off`
/// the session's effective Autopilot is Off — its own word, else the table's
/// default — an open tab whose session the harness keeps its hands off). It
/// is not the agent's switch (`on`): the switch is `[harness.<id>]`'s, for
/// every session of that agent, the word is one session's; both stop the
/// worker, neither lets it go, and `Start` waits for both:
///
/// * `TurnOff` — the word goes Off: the host STOPS the worker (it is `old`
///   until its wait ends, `Exit`), never lets it go — a let-go hands the
///   worker its agent's exit and begins the relaunch decision.
/// * `TurnOn` — the word goes back to Cruise or Land: `Start` is the host's
///   again, as for any wanted session.
/// * `Crash` — the agent exits on its own while its tab stays open. Under
///   Cruise or Land the running worker would be handed the exit; Off has no
///   running worker, so nothing is handed and nothing relaunched. `letgo`
///   records an exit handed to a worker WHILE the session is Off.
/// * `Start` waits for `off == 0` too: no worker starts for an Off session,
///   whether its agent is named again or was never stopped.
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

/// THE HOST HANDS A WORKER ITS AGENT'S EXIT ONLY FOR AN EXIT (2026-09-28):
/// `aterm-gui`'s `harness_host`, for ONE supervised session whose tab lives
/// on, under `[harness] relaunch = false` (the configuration of the test
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
/// owner's limit); `off` the agent was switched off (`[harness.<id>]
/// enabled = false`) before the host handed the exit.
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
/// * `SwitchOff` — the agent is switched off; the changed policy rings the
///   host. The model takes it only before an exit is handed (`handed == 0`):
///   a switch-off between `Visit` and `Decide` is outside it (WHAT THE MODEL
///   LEAVES OUT, below).
/// * `Visit` — a host pass, which reads the fact (`Acts::holds`) for a worker
///   the roster does not name: an agent that still holds its tab is no exit
///   — the worker is kept, and a short look is set, because nothing else
///   would ring for an exit that follows; one that holds it no longer is
///   handed the exit. A worker whose agent is switched off is stopped
///   through the switch-off path ([`harness_worker_lifecycle_model`]'s
///   `SwitchOff`): handed nothing, its agent live or not, and owed no look.
/// * `Decide` — the worker handles the exit it was handed
///   (`on_agent_left`): the upgrade's own restart in flight — asked about
///   THE LEAVING AGENT's pid (`Acts::restarted`, `relaunch::restarted`),
///   read once for every try — is carried, whatever `[harness] relaunch`
///   says; its own exit is left, and said.
///
/// `NoExitWhileAlive`: no worker is handed the exit of an agent that still
/// holds its tab. `NoExitWhileOff`: nor of an agent switched off — a stop
/// is no exit, so the agent is neither relaunched nor said. `NeverMissed`:
/// an exit is handed, a pass is still owed, or the agent is switched off.
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
/// `the_hold_reads_the_groups_leader`).
/// A switch-off after the exit was handed — between `Visit` and `Decide` —
/// is outside the model too: `SwitchOff` is guarded by `handed == 0` and
/// `Decide` is one action here, so whether such a worker still relaunches
/// its agent is the host's to bind, and nothing binds it yet.
///
/// `Buggy=1` adds five hosts, each a dead action at `Buggy=0` caught alone:
/// the host of 32a51a716, whose pass hands the exit whenever the roster does
/// not name the agent (`VisitUnconfirmed`: `Misread`, `VisitUnconfirmed` —
/// `NoExitWhileAlive`); a fix of it that keeps the worker but sets no look
/// (`VisitNoLook`: `Misread`, `VisitNoLook`, `Exit` — `NeverMissed`); the
/// worker of 32a51a716, whose exit decision reads `[harness] relaunch` alone
/// (`DecideLimited`: `Terminate`, `Visit`, `DecideLimited` —
/// `TheUpgradesOwnIsCarried`; the test failure of 32a51a716, where the
/// upgrade's own SIGTERM, both of the step's relaunch attempts waiting — the
/// shape a forced reproduction matched line for line — read as the agent
/// leaving and was dropped); and that fix's first cut, which read the
/// record by tab alone (`DecideByTab`: `Stray`, `Exit`, `Visit`,
/// `DecideByTab` — `OnlyTheUpgradesOwnIsCarried`: a person's crashed agent
/// relaunched against the owner's limit); and the host that drops a
/// switched-off agent from its roster, whose pass reads the agent as gone
/// and hands its worker the exit (`VisitOffAsLeave`: `SwitchOff`,
/// `VisitOffAsLeave` — `NoExitWhileAlive`; after an `Exit` —
/// `NoExitWhileOff`). Tier-1, in `aterm-gui`'s `harness_host` tests:
/// `the_real_host_conforms_to_the_leave_model` drives the real host through
/// every path of the environment's actions but `SwitchOff`, and checks
/// after each that what it did — kept, carried on or said — is what the
/// model does. Only `handed` and `decided` are OBSERVED there: `alive`,
/// `ours`, `stray` and `named` are what its driver did to a fake world,
/// `due` is the model's, the hold is the fake's flag (not the shipping
/// `holds`), and its negative controls fire `VisitUnconfirmed` and
/// `DecideByTab` on MODEL state alone.
/// `the_real_host_switch_off_conforms_to_the_leave_model` separately binds
/// switch-off of a named or misread live agent and its later exit: `off`
/// reads the host's live per-agent gate, its worker ends without an exit
/// decision, and another admitted agent starts after its exit as a witness
/// that a host pass reached the start decisions. `VisitOffAsLeave` itself
/// remains a MODEL-state negative control, not a runtime mutation test.
/// `a_pass_that_keeps_an_agent_owes_the_host_a_look` binds the look `Visit`
/// keeps owed (`due`) to the instant the real host's own pass (`host_pass`)
/// waits for — the one test that catches `VisitNoLook`, since a threaded
/// test host is woken by other tests' bells too.
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
///   typed just before, and it is no crash (LC-6 of ATERM-KI-1015-1): its
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
///   (LC-4 of ATERM-KI-1015-1: the told command fallen into onboarding, or
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
/// relaunched and never said. And it is the host of ATERM-KI-1015-1's LC-6
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
/// as their `/exit`), 2 read by the other rules (a graceful exit is left), 3
/// left to another supervisor that claims the session (ruling 17); `hand` —
/// ANOTHER HAND on the session, what the act's `KILL` waits out
/// (`restart_act::another_hand`): 0 none, 1 one the relaunch waits out too —
/// a hold, a driver's lease or turn (`relaunch::hand_on_tab`) — and 2
/// another supervisor's claim, which takes the exit, whether or not such a
/// hand stands beside it: the reader asks the claim first, at every look of
/// a carry (ruling 20 of 2026-10-14 and its review tell them apart:
/// `relaunch::hand_on_tab` against `claimed_by_another`); `held_kill` —
/// an act's `KILL` was sent while another hand held the session (review of
/// 1a733ed60); `into_claim` — a relaunch was typed into a tab another
/// supervisor claims (the relaunch line does not wait on a claim).
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
/// * `HandOn` / `ClaimOn` / `HandOff` — another hand lands on the session,
///   or leaves it, at any point: a fleet halt, the bridge-lost auto-hold,
///   the owner's local hold, a driver's lease or turn (`HandOn`), or another
///   supervisor's claim, `drive watch`'s (`ClaimOn`) — on a free session or
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
/// * `SelfClaimOn` / `SelfClaimOff` — a PROVEN SELF-CLAIM (the codex-alab
///   self-claim contract, §4; `selfclaim`): `P` itself claims the session,
///   the kernel proving the claim is its own, while it runs and no other
///   supervisor's claim stands (a foreign `ClaimOn` over it is refused busy,
///   as the server refuses it); it is given back, or cleared once `P` left
///   the front. It is no other hand: the act's `Stamp` and `KILL` go ahead
///   under it (the stall's remedy, which `P` cannot give itself), and once
///   `P` has ended it is NOBODY'S — `claimed_by_another` reads a proven
///   self-claim whose process is gone as no claim, so the end is `Read` by
///   this host's watch-only worker, never left to the dead holder (ruling
///   17, amended 2026-10-16). `by` — the hand that took an end left to a
///   claim (`ReadByClaim`: another supervisor's, 2).
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
/// rework, not an action. Nor what the watch reads on disk and of the host
/// before its `KILL`: it lets its program go once the tab's stamp no longer
/// names it (another act's replaced it: ruling 21 of 2026-10-14), and sends
/// it only once the host holds the snapshot the exit's reader reads it by
/// (a successor that has not read the agent yet: ruling 22) — a second
/// process and the reader's snapshot, which this model of one `P` read by
/// "whichever reader" does not have; the Tier-1s
/// `a_watch_lets_its_program_go_once_its_tabs_stamp_names_another` and
/// `a_program_stopped_across_a_commit_is_ended_once_its_successor_read_it`
/// pin them, and the Tier-1s
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
