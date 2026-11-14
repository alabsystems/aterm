// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session-owned logical intent. A worker owns attempts; the session owns its
//! cancellation even when the original vendor has exited and no PID remains.

use super::*;
use aterm_agent::harness::upgrade_drive::Opts;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub(in crate::native_act) struct Service {
    seed: Seal,
    finished: AtomicBool,
    /// The closure the root's end waited on ([`closed_frontier`]), kept for its pool's cleanup
    /// ([`unattached_completion`]): the host reclaims a completed operation once its budget has
    /// expired, so the cleanup never looks the closure up again.
    ended: Mutex<Option<Frontier>>,
    /// Who stood on the session's tab as the root ended ([`standing`]), recorded with
    /// [`Self::ended`]: whoever the hand-back finds there beside them and the root's own agent
    /// came since ([`hand_back_pass`]).
    ended_beside: Mutex<Vec<Standing>>,
}

impl Service {
    pub(in crate::native_act) fn new(seed: Seal) -> Self {
        Self {
            seed,
            finished: AtomicBool::new(false),
            ended: Mutex::new(None),
            ended_beside: Mutex::new(Vec::new()),
        }
    }

    /// The closure the root's end waited on, once it ended for want of a successor.
    #[cfg(all(test, unix))]
    pub(in crate::native_act) fn ended_closure(&self) -> Option<Arc<ClosedRestart>> {
        self.ended
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|frontier| Arc::clone(&frontier.closed))
    }

    fn pending(&self, ctx: &crate::SessionCtx) -> bool {
        !self.finished.load(Ordering::Acquire) && self.seed.bound_to_weak(ctx)
    }

    pub(super) fn persist_cancellation(
        &self,
        ctx: &crate::SessionCtx,
    ) -> Result<bool, &'static str> {
        let Some(reason) = self.seed.intent.reason() else {
            return Ok(false);
        };
        let env = self.seed.environment();
        let durable = renewal::with_receipt_writer(&env, |writer| {
            writer.cancel(
                &self.seed.receipt,
                reason,
                ctx.human_input.seq(),
                ctx.human_input.counts().reached,
            )
        })?
        .ok_or("cancellation-account-unavailable")?;
        if durable.is_some() {
            self.seed.intent.cancellation_persisted();
            super::super::reconcile_extinct(ctx);
            self.finished.store(true, Ordering::Release);
            retire_pins(ctx, &self.seed.intent);
        }
        Ok(durable.is_some())
    }
}

pub(crate) fn pending(ctx: &crate::SessionCtx) -> bool {
    ctx.fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .renewal_services
        .values()
        .any(|service| service.pending(ctx))
}

/// Called only by the confirmed human Stop Worker path. The memory fence shares
/// the native mutation boundary; account I/O and watch retirement follow it.
pub(crate) fn cancel_owner(ctx: &crate::SessionCtx) -> Result<bool, &'static str> {
    let services = {
        let _meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        table
            .renewal_services
            .values()
            .filter(|service| service.pending(ctx))
            .map(|service| {
                service.seed.cancel(renewal::CancellationKind::OwnerStop);
                Arc::clone(service)
            })
            .collect::<Vec<_>>()
    };
    let mut cancelled = false;
    let mut failed = false;
    for service in services {
        match service.persist_cancellation(ctx) {
            Ok(written) => {
                cancelled |= written;
                observe_terminal(ctx, &service.seed.receipt.id);
            }
            Err(_) => failed = true,
        }
    }
    if failed {
        Err("recovery cancellation could not be recorded")
    } else {
        Ok(cancelled)
    }
}

pub(super) fn poll_cancellation(ctx: &crate::SessionCtx, id: &str) -> Result<bool, &'static str> {
    let service = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .renewal_services
        .get(id)
        .cloned();
    let Some(service) = service else {
        return Ok(false);
    };
    service.seed.intent.current(ctx);
    service.persist_cancellation(ctx)
}

pub(super) fn observe_terminal(ctx: &crate::SessionCtx, id: &str) -> bool {
    observe_terminal_by(ctx, id, &System(&ctx.native_policy, false))
}

pub(in crate::native_act) fn observe_terminal_by(
    ctx: &crate::SessionCtx,
    id: &str,
    kernel: &dyn Kernel,
) -> bool {
    let service = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .renewal_services
        .get(id)
        .cloned();
    let Some(service) = service else { return true };
    let env = service.seed.environment();
    let Some(root) = renewal::read_receipt(&env, id) else {
        return false;
    };
    if root.phase != Phase::Committing {
        if !renewal::with_receipt_writer(&env, |writer| writer.acknowledge(&root))
            .is_ok_and(|result| result.is_some())
        {
            return false;
        }
        if !settle_terminal_native(ctx, &service.seed, kernel) {
            return false;
        }
        if !cleanup_terminal_pool(ctx, &service, &env, &root, kernel) {
            return false;
        }
        service.finished.store(true, Ordering::Release);
        retire_pins(ctx, &service.seed.intent);
        let (retired_service, retired_seals) = {
            let mut table = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let ids = table
                .renewal_seals
                .iter()
                .filter(|(_, seal)| Arc::ptr_eq(&seal.intent, &service.seed.intent))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let seals = ids
                .into_iter()
                .filter_map(|id| table.renewal_seals.remove(&id))
                .collect::<Vec<_>>();
            (table.renewal_services.remove(id), seals)
        };
        // Staged seals can own predecessor watches. Terminal intent must not
        // leave these outside native history's bounded retirement/reclamation.
        drop(retired_seals);
        drop(retired_service);
        return true;
    }
    false
}

/// Logical cancellation cannot discard pending native publication or its
/// closing reservation. Settle every operation this service actually owns.
pub(in crate::native_act) fn settle_terminal_native(
    ctx: &crate::SessionCtx,
    seed: &Seal,
    kernel: &dyn Kernel,
) -> bool {
    super::super::reconcile_extinct_by(ctx, kernel);
    let operations = {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // A writer that captured authority can still be between reservation
        // and publication. Retain its cleanup service until it exits that gap.
        if table.renewal_seals.iter().any(|(id, seal)| {
            Arc::ptr_eq(&seal.intent, &seed.intent) && table.reservations.contains_key(id)
        }) {
            return false;
        }
        table
            .by_id
            .values()
            .filter(|operation| {
                operation
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .authority
                    .renewal
                    .as_ref()
                    .is_some_and(|seal| Arc::ptr_eq(&seal.intent, &seed.intent))
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut settled = true;
    for operation in operations {
        settled &= super::super::preparation::settle_retired(ctx, operation, kernel);
    }
    settled
}

// Completion persistence can follow the restored process's first status call.
// Keep the service until its own worker can observe that proof and retire the
// exact pool record, even if the client no longer polls.
pub(in crate::native_act) fn cleanup_terminal_pool(
    ctx: &crate::SessionCtx,
    service: &Service,
    env: &Env,
    root: &Receipt,
    kernel: &dyn Kernel,
) -> bool {
    // An unreadable cancellation is uncertainty, as the writer reads it: the service waits.
    let Some(unattached) = ended_unattached(env, root) else {
        return false;
    };
    if root.intent.purpose != renewal::Purpose::WeeklyQuota {
        // A login renewal whose successor never attached hands its session back on the login it
        // left, where the same wall asks for the same successor again: its end holds that
        // successor off first ([`renewal::ReceiptWriter::note_unattached_login`]), so the session
        // is handed back only once a new root onto it would be refused.
        if unattached {
            return renewal::with_receipt_writer(env, |writer| writer.note_unattached_login(root))
                .is_ok_and(|noted| noted.is_some());
        }
        // One whose successor registered shows that login attaches: the hold-off that earlier
        // ends without a successor left on it starts over
        // ([`renewal::ReceiptWriter::note_attached_login`]).
        if matches!(root.phase, Phase::Committed | Phase::Consumed)
            && root.successor_pid.is_some()
            && root.successor_birth.is_some()
        {
            return renewal::with_receipt_writer(env, |writer| writer.note_attached_login(root))
                .is_ok_and(|noted| noted.is_some());
        }
        return true;
    }
    // A contradictory unattempted root must reach the writer's refusal, not
    // fall through as cleanup complete while its pool binding is still held.
    if root.phase == Phase::Consumed || (root.phase == Phase::Retired && !root.launch_attempted) {
        return renewal::retire_pool_record(env, root).is_ok_and(|result| result.is_some());
    }
    if root.phase == Phase::Retired
        && root.candidate.as_ref().is_some_and(|candidate| {
            !candidate.forward
                && root.successor_pid == Some(candidate.pid)
                && root.successor_birth == Some(candidate.birth)
        })
    {
        let Some(options) = opts(env, &root.sid) else {
            return false;
        };
        return renewal::with_receipt_writer(env, |writer| {
            writer.retire_rolled_back_pool_record(root, &options)
        })
        .is_ok_and(|result| result.is_some());
    }
    // A move whose successor never attached took the pool's canary onto its destination. Its
    // closure gives that back under the host's proof, or the pool stays wedged on a destination
    // no session runs on; without the proof it waits, never releasing on the receipt alone. A
    // launcher its last budget ran may have attached (the candidate) and exited unregistered.
    if unattached {
        let Some(proof) = unattached_completion(service, kernel) else {
            return false;
        };
        return renewal::with_receipt_writer(env, |writer| {
            writer.retire_unattached_rotation(root, &proof)
        })
        .is_ok_and(|result| result.is_some());
    }
    if root.phase == Phase::Retired
        && root.current_attempt.is_none()
        && root.candidate.is_none()
        && root.successor_pid.is_none()
        && root.successor_birth.is_none()
        && root.accepted_turn.is_none()
    {
        match unstarted_completion(ctx, root) {
            Unstarted::Pending => return false,
            Unstarted::Proven => {
                return renewal::with_receipt_writer(env, |writer| {
                    if renewal::read_receipt(env, &root.id).as_ref() != Some(root) {
                        return Err("cancelled-receipt-changed");
                    }
                    let mut not_started = root.clone();
                    not_started.launch_attempted = false;
                    renewal::write_receipt(env, &not_started)?;
                    writer.retire_pool_record(&not_started)
                })
                .is_ok_and(|result| result.is_some());
            }
            Unstarted::Unknown => {
                if let Some(proof) = cancelled_completion(ctx, root) {
                    return renewal::with_receipt_writer(env, |writer| {
                        writer.retire_cancelled_pool_record(root, &proof)
                    })
                    .is_ok_and(|result| result.is_some());
                }
                if root.revoked {
                    return false;
                }
            }
        }
    }
    true
}

/// Whether `root` ended because its successor never attached: retired and revoked after a
/// continuation, with no successor registered and that reason on its durable cancellation.
/// `None` where that cancellation cannot be read.
fn ended_unattached(env: &Env, root: &Receipt) -> Option<bool> {
    if root.phase != Phase::Retired
        || !root.revoked
        || root.current_attempt.is_none()
        || root.successor_pid.is_some()
        || root.successor_birth.is_some()
        || root.accepted_turn.is_some()
    {
        return Some(false);
    }
    let cancellation = renewal::read_cancellation(env, root).ok()?;
    Some(cancellation.is_some_and(|cancellation| {
        cancellation.reason == renewal::CancellationKind::SuccessorNeverAttached
    }))
}

struct CancelledCompletion<'a> {
    ctx: &'a crate::SessionCtx,
    closed: Arc<ClosedRestart>,
}

impl renewal::NativeCancellationProof for CancelledCompletion<'_> {
    fn closed_without_successor(&self, prepare: &Prepare) -> bool {
        self.closed.cancelled_without_successor(
            prepare,
            &super::super::System(&self.ctx.native_policy, false),
        )
    }
}

fn cancelled_completion<'a>(
    ctx: &'a crate::SessionCtx,
    root: &Receipt,
) -> Option<CancelledCompletion<'a>> {
    let aterm_agent::harness::native_act::Request::Prepare(prepare) =
        aterm_agent::harness::native_act::Request::parse(&root.native_prepare)?
    else {
        return None;
    };
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&prepare.id)
        .cloned()?;
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if op.request != *prepare
        || op.resume_only
        || op.phase != super::super::Phase::Completed(Decision::Retired(Retirement::NotGranted))
        || op.resume_sent
        || op.custody.is_some()
        || op.children.iter().any(Option::is_some)
        || op
            .completion_drain
            .as_ref()
            .is_some_and(|drain| !drain.is_drained())
        || op
            .resume_attempt
            .as_ref()
            .is_some_and(|attempt| !attempt.settled())
        || op
            .authority
            .renewal
            .as_ref()
            .is_none_or(|seal| seal.receipt_id() != root.id)
    {
        return None;
    }
    Some(CancelledCompletion {
        ctx,
        closed: Arc::clone(op.closed_restart.as_ref()?),
    })
}

/// The host's proof that a root's last budget closed with no successor
/// ([`renewal::NativeUnattachedProof`]), from the closure its end waited on: that budget's own
/// where the host admitted it, or, where it never did, the closure of the predecessor it was
/// staged from, which then nothing claimed. The host retires it as the cancellation persists.
pub(in crate::native_act) struct UnattachedCompletion<'a> {
    frontier: Frontier,
    kernel: &'a dyn Kernel,
}

impl renewal::NativeUnattachedProof for UnattachedCompletion<'_> {
    fn ended_without_successor(&self, prepare: &Prepare) -> bool {
        let Frontier {
            budget,
            admitted,
            closed,
        } = &self.frontier;
        prepare.id == *budget
            && if *admitted {
                closed.ended_without_successor(prepare, self.kernel)
            } else {
                closed.ended_without_successor(&closed.predecessor, self.kernel)
            }
    }
}

/// [`UnattachedCompletion`] of `service`'s root, once it ended for want of a successor.
pub(in crate::native_act) fn unattached_completion<'a>(
    service: &Service,
    kernel: &'a dyn Kernel,
) -> Option<UnattachedCompletion<'a>> {
    let frontier = service
        .ended
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    Some(UnattachedCompletion { frontier, kernel })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unstarted {
    Pending,
    Proven,
    Unknown,
}

fn unstarted_completion(ctx: &crate::SessionCtx, root: &Receipt) -> Unstarted {
    let Some(aterm_agent::harness::native_act::Request::Prepare(prepare)) =
        aterm_agent::harness::native_act::Request::parse(&root.native_prepare)
    else {
        return Unstarted::Unknown;
    };
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&prepare.id)
        .cloned();
    let Some(operation) = operation else {
        return Unstarted::Unknown;
    };
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if op.request != *prepare
        || op.resume_only
        || op
            .authority
            .renewal
            .as_ref()
            .is_none_or(|seal| seal.receipt_id() != root.id)
    {
        return Unstarted::Unknown;
    }
    if !matches!(op.phase, super::super::Phase::Completed(_)) {
        return Unstarted::Pending;
    }
    if op.phase != super::super::Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
        || op.signal_sent
        || op.resume_sent
        || op.children.iter().any(Option::is_some)
    {
        return Unstarted::Unknown;
    }
    if op.custody.is_some()
        || op
            .completion_drain
            .as_ref()
            .is_some_and(|drain| !drain.is_drained())
        || op
            .resume_attempt
            .as_ref()
            .is_some_and(|attempt| !attempt.settled())
    {
        return Unstarted::Pending;
    }
    Unstarted::Proven
}

/// A first-leg preparation can expire without ever signalling its live source.
/// Close its logical root only after the native operation positively completed.
pub(in crate::native_act) fn retire_unstarted(
    ctx: &crate::SessionCtx,
    env: &Env,
    root: &Receipt,
) -> bool {
    if root.phase != Phase::Committing
        || !root.has_no_execution_evidence()
        || unstarted_completion(ctx, root) != Unstarted::Proven
    {
        return false;
    }
    renewal::with_receipt_writer(env, |_| {
        if renewal::read_receipt(env, &root.id).as_ref() != Some(root) {
            return Err("unstarted-root-changed");
        }
        let mut closed = root.clone();
        closed.phase = Phase::Retired;
        renewal::write_receipt(env, &closed)?;
        Ok(true)
    })
    .is_ok_and(|closed| closed == Some(true))
}

fn retire_pins(ctx: &crate::SessionCtx, intent: &Arc<IntentFence>) {
    let operations = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for operation in operations {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op
            .authority
            .renewal
            .as_ref()
            .is_some_and(|seal| Arc::ptr_eq(&seal.intent, intent))
            && let Some(proof) = &op.closed_restart
        {
            proof.retire();
        }
    }
}

/// How many Resume-only continuations one root may append after its own budget closes.
/// The root's budget already spans its Forward phase and its Rollback. One continuation gives
/// a successor that was still launching, or that the host held, a whole fresh budget. A
/// second would replay the same recipe against the same host and the same launcher, so a
/// successor that has not attached by then never will. The count is the journal's attempt
/// generation, so it holds across a panic, a lost stack and a restarted worker. The last
/// budget ends its root only once it is closed as a next continuation would need it
/// ([`closed_frontier`]) and no person holds the session ([`person_holds`]): while a launcher
/// it admitted still runs, or a hold or a person's grace stands, the root waits.
pub(in crate::native_act) const MAX_CONTINUATIONS: u64 = 1;

/// Whether `root`'s active attempt is the last continuation it may append.
fn continuations_spent(root: &Receipt) -> bool {
    root.current_attempt
        .as_ref()
        .is_some_and(|attempt| attempt.generation >= MAX_CONTINUATIONS)
}

/// The host effects a closed-budget worker drives its root with ([`run_closed_by`]). A test
/// supplies its own kernel, clock and transport; production uses [`HostClosed`].
pub(in crate::native_act) trait ClosedDriver {
    /// Run `pass` with the kernel, and its clock, that native work is judged by.
    fn judged(&self, ctx: &crate::SessionCtx, pass: &mut dyn FnMut(&dyn Kernel));
    /// Reconcile an expired operation through the private protocol.
    fn reconcile(&self, opts: &Opts, sid: &str, prepare: &Prepare);
    /// Prepare one continuation and drive it Resume-only from the phase its host latched.
    fn drive(
        &self,
        opts: &Opts,
        sid: &str,
        prepare: &Prepare,
        latched: ImagePhase,
    ) -> Option<renewal::RestartCompletion>;
    /// The wait between passes.
    fn pause(&self);
    /// The coordinator's exit ([`crate::native_session_policy::SessionPolicy::renewal_coordinator_stopped`]).
    fn stopped(&self, ctx: &crate::SessionCtx, id: &str, deadline_us: u64);
    /// One step of relaunching the agent `source` names on its own line ([`hand_back`]), as
    /// the relaunch of a crashed self-claimed codex-alab answers it
    /// ([`aterm_agent::harness::upgrade_drive::renewal_hand_back`]): `wait:` while the tab
    /// cannot take the line yet, `busy:another-sweep` while another relaunch holds the sweep
    /// (any other `busy:` when the sweep's lock cannot be held), else the step it ended on.
    fn hand_back(&self, opts: &Opts, source: &self_claimed::Copy) -> String;
    /// Whether a process other than the agent `source` names, once that agent is gone, holds the
    /// writer lock of the conversation `resume` resumes
    /// ([`aterm_agent::harness::upgrade_drive::renewal_conversation_taken`]): another agent
    /// runs it, in this tab or any other.
    fn conversation_taken(
        &self,
        source: &self_claimed::Copy,
        resume: &self_claimed::Resume,
    ) -> bool;
}

pub(super) struct HostClosed;

impl ClosedDriver for HostClosed {
    fn judged(&self, ctx: &crate::SessionCtx, pass: &mut dyn FnMut(&dyn Kernel)) {
        pass(&System(&ctx.native_policy, false));
    }

    fn reconcile(&self, opts: &Opts, sid: &str, prepare: &Prepare) {
        // This invokes production reconciliation through the same private
        // protocol. Missing/stale files never establish a closed witness.
        renewal::reconcile_restart(opts, sid, prepare);
    }

    fn drive(
        &self,
        opts: &Opts,
        sid: &str,
        prepare: &Prepare,
        latched: ImagePhase,
    ) -> Option<renewal::RestartCompletion> {
        renewal::prepare_restart(opts, sid, prepare).ok()?;
        renewal::drive_resume_only(opts, sid, prepare, latched)
    }

    fn pause(&self) {
        pause();
    }

    fn stopped(&self, ctx: &crate::SessionCtx, id: &str, deadline_us: u64) {
        ctx.native_policy
            .renewal_coordinator_stopped(id, deadline_us);
    }

    fn hand_back(&self, opts: &Opts, source: &self_claimed::Copy) -> String {
        let Ok(resume) = &source.resume else {
            return "refused:no-resume-line".into();
        };
        match aterm_agent::harness::relaunch::self_claimed_snapshot(opts, source, resume) {
            Ok(snapshot) => {
                aterm_agent::harness::upgrade_drive::renewal_hand_back(opts, &snapshot, resume).step
            }
            Err(why) => unsnapped_step(why),
        }
    }

    fn conversation_taken(
        &self,
        source: &self_claimed::Copy,
        resume: &self_claimed::Resume,
    ) -> bool {
        aterm_agent::harness::upgrade_drive::renewal_conversation_taken(source, resume)
    }
}

/// The hand-back's step where the tab could not be read as the shell's ([`HostClosed`]). The
/// shell holds the tab again once the last launcher is gone, and a read of the roster, the
/// shell's terminal or its arguments can fail for now: each waits. Only a tab this relaunch can
/// never name is refused: no tab to name, or a shell another tab started.
fn unsnapped_step(why: &str) -> String {
    match why {
        "no-tab" | "tab-identity-conflict" => format!("refused:{why}"),
        _ => format!("wait:{why}"),
    }
}

/// The commit worker's end: drive every closed-budget child, observe the root's terminal
/// receipt and report the coordinator's exit at the active budget's deadline.
pub(super) fn close_worker(
    weak: &std::sync::Weak<crate::SessionCtx>,
    opts: &Opts,
    env: &Env,
    id: &str,
    root_deadline_us: u64,
) {
    close_worker_by(weak, opts, env, id, root_deadline_us, &HostClosed);
}

pub(in crate::native_act) fn close_worker_by(
    weak: &std::sync::Weak<crate::SessionCtx>,
    opts: &Opts,
    env: &Env,
    id: &str,
    root_deadline_us: u64,
    driver: &dyn ClosedDriver,
) {
    // This worker alone hands its root's session back, so it keeps the service past its end.
    let service = weak.upgrade().and_then(|ctx| {
        ctx.fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .renewal_services
            .get(id)
            .cloned()
    });
    continue_closed_by(weak, opts, id, driver);
    if let Some(ctx) = weak.upgrade() {
        driver.judged(&ctx, &mut |kernel| {
            let _terminal = observe_terminal_by(&ctx, id, kernel);
        });
        let deadline = renewal::read_receipt(env, id)
            .and_then(|current| renewal::active_prepare(env, &current))
            .map_or(root_deadline_us, |active| active.deadline_us);
        driver.stopped(&ctx, id, deadline);
    }
    if let Some(service) = service {
        hand_back(weak, &service, opts, env, id, driver);
    }
}

/// How long a hand-back waits on the tab once nothing defers it ([`hand_back`]): the root's
/// service finishing, the shell's prompt, the shell holding the tab again, and the relaunch's
/// own waits for the new agent to lead the tab and reach its first idle, which take a few
/// minutes at most. Another tab's relaunch holding the sweep defers it without counting.
const HAND_BACK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What one pass of [`hand_back`] leaves it to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Back {
    /// It is over: the line was typed, or the session is not this worker's to hand back.
    Over,
    /// The tab cannot take the line yet: wait, and count the wait.
    Wait,
    /// Another relaunch holds the sweep: wait without counting.
    Defer,
}

/// Who holds a session's tab, as the hand-back reads it ([`standing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Standing {
    /// A proven agent: a live claim's proof, or the copy a proven self-claim left
    /// ([`crate::self_claim::copy_of_tab`]).
    Agent(crate::self_claim::Proven),
    /// A live claim that proves no agent, by a supervisor other than aterm's harness.
    Holder(String),
}

/// Who stands on `ctx`'s tab now: its live claim, and the copy a proven self-claim left there.
fn standing(ctx: &crate::SessionCtx) -> Vec<Standing> {
    let mut standing = Vec::new();
    let claim = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(crate::metrics::now_us())
            .map(|claim| (claim.holder.clone(), claim.identity().cloned()))
    };
    match claim {
        Some((_, Some(proven))) => standing.push(Standing::Agent(proven)),
        Some((holder, None)) if !aterm_agent::harness::upgrade_drive::is_host_holder(&holder) => {
            standing.push(Standing::Holder(holder));
        }
        Some(_) | None => {}
    }
    if let Some(copy) = crate::self_claim::copy_of_tab(ctx.self_id.as_str()) {
        standing.push(Standing::Agent(crate::self_claim::Proven {
            pid: copy.pid,
            start: copy.start,
        }));
    }
    standing
}

/// A root that ended because its successor never attached signalled its session's agent, and
/// no successor took the session: the crash rule judged that SIGTERM the agent's own end, so
/// nothing else relaunches it. Its worker hands the session back once, after the end settled
/// every operation and gave back what the move took (the service finished, which it observes
/// again while it has not): it relaunches the agent the Signal ended on its own line, the
/// conversation, login and home it ran on as its copy was validated at commit, by the crash
/// rule's relaunch of a self-claimed codex-alab, filed as the renewal's. That relaunch records a
/// witness of the agent once it typed the line, and the hand-back ends there, whatever the step
/// said: the agent it started is the harness's to supervise, and no later relaunch types the
/// line again. It hands back only a session that is still the root's own ([`taken_since`]): a
/// person's hold, a person's gesture since the end, another agent or supervisor on the tab, or
/// another process on the conversation stops it, as a person's keys or another's claim leave a
/// crashed agent's exit to them; so does the tab's end. Another tab's relaunch defers it without
/// bound; other waits count against [`HAND_BACK_PATIENCE`].
fn hand_back(
    weak: &std::sync::Weak<crate::SessionCtx>,
    service: &Service,
    opts: &Opts,
    env: &Env,
    id: &str,
    driver: &dyn ClosedDriver,
) {
    if !service.seed.intent.durably_cancelled()
        || service.seed.intent.reason() != Some(renewal::CancellationKind::SuccessorNeverAttached)
    {
        return;
    }
    let Some(source) = &service.seed.source else {
        aterm_log::info!("codex renewal {id}: no source line to hand the session back on");
        return;
    };
    let Ok(resume) = &source.resume else {
        aterm_log::info!("codex renewal {id}: its source has no resume line to hand back on");
        return;
    };
    let witnesses = self_claimed::Witnesses::under(&opts.state);
    let mut waited = std::time::Duration::ZERO;
    loop {
        let began = std::time::Instant::now();
        // The tab's context is held only for one pass: a closed tab ends the hand-back.
        let Some(ctx) = weak.upgrade() else { return };
        let back = hand_back_pass(
            &ctx, &witnesses, service, source, resume, opts, env, id, driver,
        );
        drop(ctx);
        if back == Back::Over {
            return;
        }
        driver.pause();
        if back == Back::Wait {
            waited = waited.saturating_add(began.elapsed());
            if waited >= HAND_BACK_PATIENCE {
                aterm_log::info!(
                    "codex renewal {id}: the session's agent {} was not handed back: the tab \
                     could not take its line within {}s",
                    source.pid,
                    HAND_BACK_PATIENCE.as_secs()
                );
                return;
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one pass of hand_back, which holds every piece across passes"
)]
fn hand_back_pass(
    ctx: &crate::SessionCtx,
    witnesses: &self_claimed::Witnesses,
    service: &Service,
    source: &self_claimed::Copy,
    resume: &self_claimed::Resume,
    opts: &Opts,
    env: &Env,
    id: &str,
    driver: &dyn ClosedDriver,
) -> Back {
    if witnesses.handled(source.pid, &source.start) {
        aterm_log::info!(
            "codex renewal {id}: the session's agent {} was relaunched on its own line",
            source.pid
        );
        return Back::Over;
    }
    let serving = || {
        ctx.fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .renewal_services
            .contains_key(id)
    };
    if serving() {
        driver.judged(ctx, &mut |kernel| {
            let _terminal = observe_terminal_by(ctx, id, kernel);
        });
        if serving() {
            return Back::Wait;
        }
    }
    let Some(root) = renewal::read_receipt(env, id).filter(|root| {
        root.phase == Phase::Retired
            && root.successor_pid.is_none()
            && root.successor_birth.is_none()
            && root.accepted_turn.is_none()
    }) else {
        return Back::Over;
    };
    let ended = match renewal::read_cancellation(env, &root) {
        Ok(Some(ended)) => ended,
        Ok(None) => return Back::Over,
        Err(_) => return Back::Wait,
    };
    if let Some(whose) = taken_since(ctx, service, &ended, source, resume, driver) {
        aterm_log::info!(
            "codex renewal {id}: {whose}; the session's agent {} is not handed back",
            source.pid
        );
        return Back::Over;
    }
    // The line the relaunched agent inherits until its own record names one, as the crash
    // rule's relaunch records it.
    crate::self_claim::typed(ctx.self_id.as_str(), resume);
    let step = driver.hand_back(opts, source);
    let back = if witnesses.handled(source.pid, &source.start) {
        Back::Over
    } else {
        step_back(&step)
    };
    if back == Back::Over {
        aterm_log::info!(
            "codex renewal {id}: hand-back to the session's agent {}: {step}",
            source.pid
        );
    }
    back
}

/// What a hand-back step whose witness did not end the hand-back leaves it to do: another
/// relaunch holding the machine's sweep (`busy:another-sweep`) defers it; any other `busy:` (the
/// state directory cannot hold the sweep's lock, which may not clear) and every `wait:` count
/// against [`HAND_BACK_PATIENCE`]; any other step ended it.
fn step_back(step: &str) -> Back {
    if step == "busy:another-sweep" {
        Back::Defer
    } else if step.starts_with("wait:") || step.starts_with("busy:") {
        Back::Wait
    } else {
        Back::Over
    }
}

/// Why the session is no longer only the ended root's to hand back, if it is not: someone took
/// it since the end. A person's hold; a person's gesture since the cancellation counted theirs
/// (`ended`'s `human_seq`: the end waited until no person's grace stood, so every later one is
/// after it), as a person's keys make a crashed agent's exit theirs; an agent or supervisor on
/// the tab that neither stood there at the end ([`Service::ended_beside`]) nor is the root's
/// own (its claimant's proof, its source); a process other than the gone source on the
/// conversation, from any tab. Each is evidence that outlasts the hand-back's next pass, so a
/// session someone used and left is not resumed under them.
fn taken_since(
    ctx: &crate::SessionCtx,
    service: &Service,
    ended: &renewal::Cancellation,
    source: &self_claimed::Copy,
    resume: &self_claimed::Resume,
    driver: &dyn ClosedDriver,
) -> Option<String> {
    if ctx.fabric.hold().is_some() {
        return Some("a person holds the session".into());
    }
    if ctx.human_input.seq() != ended.human_seq {
        return Some("a person used the session after its root ended: it is theirs".into());
    }
    let beside = service
        .ended_beside
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let own = |who: &Standing| match who {
        Standing::Agent(proven) => {
            *proven == service.seed.proof
                || (proven.pid == source.pid && proven.start == source.start)
        }
        Standing::Holder(_) => false,
    };
    if let Some(who) = standing(ctx)
        .into_iter()
        .find(|who| !own(who) && !beside.contains(who))
    {
        return Some(match who {
            Standing::Agent(proven) => format!(
                "the agent {} (started {}) took the tab after its root ended",
                proven.pid, proven.start
            ),
            Standing::Holder(holder) => {
                format!("{holder} claimed the tab after its root ended")
            }
        });
    }
    if driver.conversation_taken(source, resume) {
        return Some(format!(
            "another process runs its conversation {}",
            resume.session
        ));
    }
    None
}

/// The original receipt CAS starts one worker. That same worker owns every
/// append-only child, including time spent held between native budgets.
fn continue_closed_by(
    weak: &std::sync::Weak<crate::SessionCtx>,
    opts: &Opts,
    id: &str,
    driver: &dyn ClosedDriver,
) {
    loop {
        // A lost stack never becomes a new logical worker or guessed child.
        // Reload the authoritative root and exact retained attempt after panic.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_closed_by(weak, opts, id, driver);
        }))
        .is_ok()
        {
            return;
        }
        driver.pause();
    }
}

/// What one pass of [`run_closed_by`] leaves its loop to do.
enum Step {
    /// The root is terminal or its service is gone: the worker ends.
    Done,
    /// The pass changed the root: reload it at once.
    Again,
    /// Nothing more is possible now: wait, then reload.
    Pause,
}

fn run_closed_by(
    weak: &std::sync::Weak<crate::SessionCtx>,
    opts: &Opts,
    id: &str,
    driver: &dyn ClosedDriver,
) {
    loop {
        let Some(ctx) = weak.upgrade() else { return };
        let mut step = Step::Pause;
        driver.judged(&ctx, &mut |kernel| {
            step = closed_pass(&ctx, opts, id, kernel, driver);
        });
        drop(ctx);
        match step {
            Step::Done => return,
            Step::Again => {}
            Step::Pause => driver.pause(),
        }
    }
}

fn closed_pass(
    ctx: &Arc<crate::SessionCtx>,
    opts: &Opts,
    id: &str,
    kernel: &dyn Kernel,
    driver: &dyn ClosedDriver,
) -> Step {
    let service = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .renewal_services
        .get(id)
        .cloned();
    let Some(service) = service else {
        return Step::Done;
    };
    match poll_cancellation(ctx, id) {
        Ok(true) => {
            return if observe_terminal_by(ctx, id, kernel) {
                Step::Done
            } else {
                Step::Pause
            };
        }
        Err(_) => return Step::Pause,
        Ok(false) => {}
    }
    let env = service.seed.environment();
    let Some(root) = renewal::read_receipt(&env, id) else {
        return Step::Pause;
    };
    if root.phase != Phase::Committing || root.revoked || root.accepted_turn.is_some() {
        return if observe_terminal_by(ctx, id, kernel) {
            Step::Done
        } else {
            Step::Pause
        };
    }
    if !root.same_intent(&service.seed.receipt) {
        let _first = service
            .seed
            .cancel(renewal::CancellationKind::ChangedRecord);
        return Step::Pause;
    }
    let Some(prepare) = renewal::active_prepare(&env, &root) else {
        return Step::Pause;
    };
    if !root_allows_native_work(&env, &root, &prepare, opts) {
        return Step::Pause;
    }
    let Some(now) = kernel.clock().now_us() else {
        return Step::Pause;
    };
    if now >= prepare.deadline_us {
        driver.reconcile(opts, &root.sid, &prepare);
        if retire_unstarted(ctx, &env, &root) {
            return Step::Again;
        }
        if continuations_spent(&root) {
            // The last budget ends the root only once it closed with no successor live, and
            // never while a person holds the session. Until then it waits, as for one more.
            if person_holds(ctx, &env) {
                return Step::Pause;
            }
            let Ok(frontier) = closed_frontier(ctx, &root, &env, &prepare, kernel) else {
                return Step::Pause;
            };
            // It ends once, as the durable cancellation the next pass writes and settles, with
            // the person's count at the end. The closure it waited on stays with the service for
            // the pool's cleanup, and who stood on the tab then for the hand-back.
            *service.ended.lock().unwrap_or_else(|p| p.into_inner()) = Some(frontier);
            *service
                .ended_beside
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = standing(ctx);
            if service
                .seed
                .cancel(renewal::CancellationKind::SuccessorNeverAttached)
            {
                aterm_log::info!(
                    "codex renewal {id}: successor-never-attached after {MAX_CONTINUATIONS} continuation; its worker hands the session back"
                );
            }
            return Step::Again;
        }
        let _attempt = append_child(ctx, &service, &root, &env, &prepare, kernel);
    } else if let Some(reference) = &root.current_attempt {
        let admitted = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_id
            .get(&reference.id)
            .cloned();
        // The host latches a continuation in its predecessor's closed phase; this drive
        // starts there, so it never asks a Rollback-closed host for Forward.
        let latched = if let Some(operation) = admitted {
            admitted_phase(&operation, &prepare)
        } else {
            renewal::read_active_attempt(&env, &root)
                .ok()
                .flatten()
                .and_then(|attempt| {
                    let (_, proof) = closed_predecessor(ctx, &attempt.native_predecessor, kernel)?;
                    stage_continuation(ctx, &root, &attempt, kernel).ok()?;
                    Some(proof.closed_phase())
                })
        };
        if let Some(latched) = latched {
            let observed = driver.drive(opts, &root.sid, &prepare, latched);
            publish_completion(&env, &root, observed);
        }
    }
    Step::Pause
}

/// The phase the host latched an admitted continuation in, when it admitted exactly `prepare`.
/// The operation's lock is held only here, never across the predecessor's lookup.
fn admitted_phase(operation: &Mutex<Operation>, prepare: &Prepare) -> Option<ImagePhase> {
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    (op.request == *prepare).then(|| op.resume_phase.unwrap_or(ImagePhase::Forward))
}

/// A root can finish after its original worker hands off to this service.
/// Consume that completion before considering any deadline or host operation.
/// An unreadable, contended or pending journal holds the service in place.
pub(super) fn root_allows_native_work(
    env: &Env,
    root: &Receipt,
    prepare: &Prepare,
    opts: &aterm_agent::harness::upgrade_drive::Opts,
) -> bool {
    if root.current_attempt.is_some() {
        return true;
    }
    matches!(
        renewal::with_receipt_writer(env, |writer| {
            writer.recover_completion(root, prepare, opts)
        }),
        Ok(Some(
            renewal::CompletionProgress::Absent | renewal::CompletionProgress::Active
        ))
    )
}

fn pause() {
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// Whether a person holds this session's renewal work now: a fabric hold, or a person's grace.
fn person_holds(ctx: &crate::SessionCtx, env: &Env) -> bool {
    ctx.fabric.hold().is_some() || !human_grace_ready(ctx, env)
}

/// A closed budget ([`closed_frontier`]): the budget, and the host's closure of the native
/// predecessor a next attempt would start from, which is the budget's own where the host
/// admitted it.
#[derive(Clone, Debug)]
pub(in crate::native_act) struct Frontier {
    budget: String,
    admitted: bool,
    closed: Arc<ClosedRestart>,
}

impl Frontier {
    /// The native predecessor a next attempt starts from.
    fn predecessor(&self) -> &str {
        &self.closed.predecessor.id
    }
}

/// The closure `root`'s next attempt would start from, once `previous`'s budget is closed: it
/// expired and nothing it admitted is live. Its predecessor is `previous` itself when the host
/// closed it, or, when the host never admitted it, the closed predecessor it was staged from.
fn closed_frontier(
    ctx: &crate::SessionCtx,
    root: &Receipt,
    env: &Env,
    previous: &Prepare,
    kernel: &dyn Kernel,
) -> Result<Frontier, &'static str> {
    if !kernel
        .clock()
        .now_us()
        .is_some_and(|now| now >= previous.deadline_us)
    {
        return Err("continuation-budget-live");
    }
    if let Some((_, closed)) = closed_predecessor(ctx, &previous.id, kernel) {
        return Ok(Frontier {
            budget: previous.id.clone(),
            admitted: true,
            closed,
        });
    }
    let current = renewal::read_active_attempt(env, root)?.ok_or("continuation-no-attempt")?;
    // Only an expired attempt that was never admitted may be skipped. A
    // live or completed unknown capability is reconciled, never replaced.
    let table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if table.by_id.contains_key(&previous.id) {
        return Err("continuation-predecessor-unresolved");
    }
    drop(table);
    let (_, closed) = closed_predecessor(ctx, &current.native_predecessor, kernel)
        .ok_or("continuation-predecessor-unresolved")?;
    Ok(Frontier {
        budget: previous.id.clone(),
        admitted: false,
        closed,
    })
}

pub(in crate::native_act) fn append_child(
    ctx: &Arc<crate::SessionCtx>,
    service: &Service,
    root: &Receipt,
    env: &Env,
    previous: &Prepare,
    kernel: &dyn Kernel,
) -> Result<(), &'static str> {
    if !service.seed.intent_active()
        || person_holds(ctx, env)
        || !service
            .seed
            .record
            .as_ref()
            .is_some_and(|record| record.current(root))
    {
        return Err("continuation-held");
    }
    let frontier = closed_frontier(ctx, root, env, previous, kernel)?;
    let snapshot = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    let child = Prepare {
        asked: None,
        id: aterm_agent::harness::native_act::operation_id().map_err(|_| "continuation-entropy")?,
        revision: snapshot.revision(),
        expected_human_seq: Some(ctx.human_input.seq()),
        deadline_us: kernel
            .clock()
            .now_us()
            .and_then(|now| now.checked_add(MAX_PREPARE_US))
            .ok_or("continuation-clock")?,
        ..previous.clone()
    };
    let published = renewal::with_selected_writer(env, root, |next, writer| {
        if !root.matches_successor(next) {
            return Err("continuation-selection-changed");
        }
        let attempt = writer.propose(root, frontier.predecessor(), &child)?;
        writer.publish(root, &attempt)
    });
    match published {
        Ok(Some(current))
            if current.phase == Phase::Committing
                && !current.revoked
                && current.accepted_turn.is_none() =>
        {
            Ok(())
        }
        Err("continuation-selection-changed") => {
            service
                .seed
                .cancel(renewal::CancellationKind::ChangedSelection);
            Err("continuation-selection-changed")
        }
        _ => Err("continuation-publication-unresolved"),
    }
}

fn publish_completion(env: &Env, expected: &Receipt, observed: Option<renewal::RestartCompletion>) {
    let Some(observed) = observed else { return };
    let _saved = renewal::with_receipt_writer(env, |_| {
        let Some(mut current) = renewal::read_receipt(env, &expected.id) else {
            return Err("completion-root");
        };
        if !current.same_intent(expected)
            || current.current_attempt != expected.current_attempt
            || current.phase != Phase::Committing
            || current.revoked
            || current.accepted_turn.is_some()
        {
            return Err("completion-changed");
        }
        match observed {
            renewal::RestartCompletion::Forward(observation) => {
                current.phase = Phase::Committed;
                current.successor_pid = Some(observation.registration.vendor.pid);
                current.successor_birth = Some(observation.registration.vendor.birth);
            }
            renewal::RestartCompletion::Rollback(observation) => {
                current.phase = Phase::Retired;
                current.successor_pid = Some(observation.registration.vendor.pid);
                current.successor_birth = Some(observation.registration.vendor.birth);
            }
            renewal::RestartCompletion::NotStarted => return Err("continuation-unexpected-signal"),
        }
        renewal::write_receipt(env, &current)
    });
}

#[cfg(test)]
#[path = "codex_renewal_service_tests.rs"]
mod tests;
