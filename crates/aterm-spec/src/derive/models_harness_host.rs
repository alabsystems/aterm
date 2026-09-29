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
/// its step is running (it stays queued meanwhile); `paused` — a handoff
/// parked the readers; `carried` — `A` is on the handoff layout the park
/// froze; `committed` — the successor took over (the outgoing instance and
/// any step it was running end there); `succ` — the successor's own queue
/// owes `A`; `resolved` — `A` was relaunched or said; `late` — a step that
/// STARTED while parked; `restart` — `R`'s record is in flight; `swept` —
/// the successor's Commit owes `R` a carry-on step.
///
/// * `StepStart`, `StepRetries`, `StepResolves` — the outgoing worker's step
///   and its two ends (a word that is not possible yet keeps `A` queued);
///   no step STARTS while parked.
/// * `Park` freezes the queue AS IT STANDS, the step in flight included;
///   `Rollback` resumes it.
/// * `Commit` hands `carried` to the successor's queue and owes every
///   restart record still in flight a carry-on.
/// * `SuccRelaunches`, `SweepCarriesOn` — the successor's steps (each the
///   same idempotent step the outgoing instance would have taken, so a
///   relaunch it already typed is carried on, never typed twice).
/// * `RestartFinishes` — the outgoing instance's own act finished `R` before
///   the Commit.
///
/// * `NoSilentLoss` — past the Commit `A` is resolved or the successor owes
///   it: the reopened layout's row said it resumes;
/// * `NoStepWhileParked` — the worker takes no new step while the terminal is
///   parked;
/// * `NoStrandedRestart` — past the Commit a restart record in flight is owed
///   a carry-on.
///
/// `Buggy=1` is what shipped until round four, each alone: `ParkDropsQueue`
/// (the queue was the worker's local and the handoff leaf never named an
/// agent — `NoSilentLoss`), `StepStartWhileParked` (a worker the park does
/// not stop — `NoStepWhileParked`) and `CommitWithoutSweep` (the successor
/// resumed its host for agent tabs only — `NoStrandedRestart`). The bounded
/// hold that keeps an AUTOMATIC update from parking while `A` is queued is
/// the ladder's (`native_update_apply_ladder_model`'s `warmup`); this machine
/// is what an explicit update, and one past the hold's bound, relies on.
/// Tier-1 (`aterm-gui`'s `harness_host` test
/// `the_real_restored_carry_conforms_to_its_model`) drives the real host's
/// queue through a park mid-step, a successor's relaunch of what it carried
/// and the Commit's sweep, projecting each observed state onto the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restored_carry_model() -> Model {
    crate::ty_model! {
        HarnessRestoredCarry {
            const Buggy = 0;
            var queued = 1;
            var acting = 0;
            var paused = 0;
            var carried = 0;
            var committed = 0;
            var succ = 0;
            var resolved = 0;
            var late = 0;
            var restart = 1;
            var swept = 0;

            action StepStart when (queued == 1 && acting == 0 && paused == 0 && committed == 0) {
                acting = 1;
            }
            action StepStartWhileParked when (
                Buggy == 1 && queued == 1 && acting == 0 && paused == 1 && committed == 0
            ) {
                acting = 1;
                late = 1;
            }
            action StepRetries when (acting == 1 && committed == 0) {
                acting = 0;
            }
            action StepResolves when (acting == 1 && committed == 0) {
                acting = 0;
                queued = 0;
                resolved = 1;
            }
            action RestartFinishes when (restart == 1 && committed == 0) {
                restart = 0;
            }
            action Park when (paused == 0 && committed == 0) {
                paused = 1;
                carried = queued;
            }
            action ParkDropsQueue when (Buggy == 1 && paused == 0 && committed == 0) {
                paused = 1;
                carried = 0;
            }
            action Rollback when (paused == 1 && committed == 0) {
                paused = 0;
                carried = 0;
            }
            action Commit when (paused == 1 && committed == 0) {
                committed = 1;
                succ = carried;
                queued = 0;
                acting = 0;
                swept = restart;
            }
            action CommitWithoutSweep when (Buggy == 1 && paused == 1 && committed == 0) {
                committed = 1;
                succ = carried;
                queued = 0;
                acting = 0;
                swept = 0;
            }
            action SuccRelaunches when (committed == 1 && succ == 1) {
                succ = 0;
                resolved = 1;
            }
            action SweepCarriesOn when (committed == 1 && swept == 1) {
                swept = 0;
                restart = 0;
            }

            invariant NoSilentLoss: committed == 0 || resolved == 1 || succ == 1;
            invariant NoStepWhileParked: late == 0;
            invariant NoStrandedRestart: committed == 0 || restart == 0 || swept == 1;
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
/// * `Restart` — the pause ends: the next run starts, and the badge goes.
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
/// (`NeverGivesUp`: a badged session is still supervised, or its badged
/// worker is on its way out). The shipped
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
/// `word` the shell has drawn the status; `seen` the look has decided and
/// `kept` what it found (1 a crash, 2 no crash, 3 no word within its wait);
/// `decided` 0 no attempt yet, 1 relaunched, 2 left.
///
/// `TheirsIsLeft`: an own exit or someone's signal is never relaunched.
/// `AToldCrashIsRelaunched`: a crash is left only when no word came.
///
/// `Buggy = 1` adds two looks, each dead at `Buggy = 0` and caught alone:
/// the first cut's, which read the lock whenever the shell had not drawn
/// its word yet (`LookAtLock`: `Signal`, `LookAtLock`, `Decide` relaunches
/// a person's `kill` — `TheirsIsLeft`), and Claude Code's rule inherited,
/// silence read at once as a graceful exit (`LookAsClaude`: `Crash`,
/// `LookAsClaude`, `Decide` leaves a crash — `AToldCrashIsRelaunched`).
/// Tier-1 (aterm-agent `harness/upgrade_codex_drive_tests.rs`,
/// `tier1_the_real_codex_exit_read_conforms_to_the_model`) replays every
/// look of the model through the real `look_at_exit`, `exit_record` and
/// `after_exit` over a stand-in tab, the lock on disk as the model has it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_codex_exit_witness_model() -> Model {
    crate::ty_model! {
        HarnessCodexExitWitness {
            const Buggy = 0;
            var exit = 0;
            var lock = 1;
            var word = 0;
            var seen = 0;
            var kept = 0;
            var decided = 0;

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
            action Look when (exit > 0 && seen == 0 && word == 1) {
                seen = 1;
                kept = if exit == 1 { 1 } else { 2 };
            }
            action LookGone when (exit > 0 && seen == 0 && word == 0) {
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
                decided = if kept == 1 { 1 } else { 2 };
            }

            invariant TheirsIsLeft: exit <= 1 || decided == 0 || decided == 2;
            invariant AToldCrashIsRelaunched: exit == 0 || exit > 1 || decided <= 1 || kept == 3;
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
