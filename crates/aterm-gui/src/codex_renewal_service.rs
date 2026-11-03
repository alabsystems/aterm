// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session-owned logical intent. A worker owns attempts; the session owns its
//! cancellation even when the original vendor has exited and no PID remains.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub(in crate::native_act) struct Service {
    seed: Seal,
    finished: AtomicBool,
}

impl Service {
    pub(in crate::native_act) fn new(seed: Seal) -> Self {
        Self {
            seed,
            finished: AtomicBool::new(false),
        }
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
        }
        self.finished.store(true, Ordering::Release);
        retire_pins(ctx, &self.seed.intent);
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

pub(super) fn observe_terminal(ctx: &crate::SessionCtx, id: &str) {
    let service = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .renewal_services
        .get(id)
        .cloned();
    let Some(service) = service else { return };
    let env = service.seed.environment();
    if renewal::read_receipt(&env, id).is_some_and(|root| root.phase != Phase::Committing) {
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
    }
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

/// The original receipt CAS starts one worker. That same worker owns every
/// append-only child, including time spent held between native budgets.
pub(super) fn continue_closed(
    weak: &std::sync::Weak<crate::SessionCtx>,
    opts: &aterm_agent::harness::upgrade_drive::Opts,
    id: &str,
) {
    loop {
        // A lost stack never becomes a new logical worker or guessed child.
        // Reload the authoritative root and exact retained attempt after panic.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_closed(weak, opts, id)))
            .is_ok()
        {
            return;
        }
        pause();
    }
}

fn run_closed(
    weak: &std::sync::Weak<crate::SessionCtx>,
    opts: &aterm_agent::harness::upgrade_drive::Opts,
    id: &str,
) {
    loop {
        let Some(ctx) = weak.upgrade() else { return };
        let service = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .renewal_services
            .get(id)
            .cloned();
        let Some(service) = service else { return };
        match poll_cancellation(&ctx, id) {
            Ok(true) => {
                observe_terminal(&ctx, id);
                return;
            }
            Err(_) => {
                drop(ctx);
                pause();
                continue;
            }
            Ok(false) => {}
        }
        let env = service.seed.environment();
        let Some(root) = renewal::read_receipt(&env, id) else {
            drop(ctx);
            pause();
            continue;
        };
        if root.phase != Phase::Committing || root.revoked || root.accepted_turn.is_some() {
            observe_terminal(&ctx, id);
            return;
        }
        if !root.same_intent(&service.seed.receipt) {
            service
                .seed
                .cancel(renewal::CancellationKind::ChangedRecord);
            drop(ctx);
            pause();
            continue;
        }
        let Some(prepare) = renewal::active_prepare(&env, &root) else {
            drop(ctx);
            pause();
            continue;
        };
        let Some(now) = aterm_agent::harness::native_act::monotonic_us() else {
            drop(ctx);
            pause();
            continue;
        };
        if now >= prepare.deadline_us {
            // This invokes production reconciliation through the same private
            // protocol. Missing/stale files never establish a closed witness.
            renewal::reconcile_restart(opts, &root.sid, &prepare);
            let _attempt = append_child(
                &ctx,
                &service,
                &root,
                &env,
                &prepare,
                &System(&ctx.native_policy, false),
            );
        } else if let Some(reference) = &root.current_attempt {
            let admitted = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .by_id
                .get(&reference.id)
                .cloned();
            let valid = if let Some(operation) = admitted {
                operation.lock().unwrap_or_else(|p| p.into_inner()).request == prepare
            } else {
                renewal::read_active_attempt(&env, &root)
                    .ok()
                    .flatten()
                    .is_some_and(|attempt| {
                        stage_continuation(
                            &ctx,
                            &root,
                            &attempt,
                            &System(&ctx.native_policy, false),
                        )
                        .is_ok()
                    })
            };
            if valid && renewal::prepare_restart(opts, &root.sid, &prepare).is_ok() {
                let observed = renewal::drive_resume_only(opts, &root.sid, &prepare);
                publish_completion(&env, &root, observed);
            }
        }
        drop(ctx);
        pause();
    }
}

fn pause() {
    std::thread::sleep(std::time::Duration::from_millis(100));
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
        || ctx.fabric.hold().is_some()
        || !human_grace_ready(ctx, env)
        || !service
            .seed
            .record
            .as_ref()
            .is_some_and(|record| record.current(root))
    {
        return Err("continuation-held");
    }
    if !kernel
        .clock()
        .now_us()
        .is_some_and(|now| now >= previous.deadline_us)
    {
        return Err("continuation-budget-live");
    }
    let predecessor = if closed_predecessor(ctx, &previous.id, kernel).is_some() {
        previous.id.clone()
    } else {
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
        if closed_predecessor(ctx, &current.native_predecessor, kernel).is_none() {
            return Err("continuation-predecessor-unresolved");
        }
        current.native_predecessor
    };
    let snapshot = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    let child = Prepare {
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
    let published = renewal::with_successor_writer(
        env,
        &root.old_auth,
        &root.intent.identity,
        |next, writer| {
            if !root.matches_successor(next) {
                return Err("continuation-selection-changed");
            }
            let attempt = writer.propose(root, &predecessor, &child)?;
            writer.publish(root, &attempt)
        },
    );
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
