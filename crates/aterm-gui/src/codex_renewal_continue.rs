// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A new, sealed Resume-only admission starts from a closed native operation,
//! never from a missing PID or a disk journal. It grants no second Signal.

use super::*;

pub(in crate::native_act) fn closed_predecessor(
    ctx: &crate::SessionCtx,
    id: &str,
    kernel: &dyn Kernel,
) -> Option<(Arc<Mutex<Operation>>, Arc<ClosedRestart>)> {
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(id)?
        .clone();
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    let no_go = matches!(
        op.phase,
        super::super::Phase::Completed(Decision::Retired(_))
    ) || (op.resume_only
        && op.phase == super::super::Phase::Completed(Decision::ProvenNotCommittedRollbackSafe));
    if !no_go
        || op
            .authority
            .renewal
            .as_ref()
            .is_none_or(|seal| !seal.bound_to_weak(ctx) || !seal.intent_active())
    {
        return None;
    }
    if op.closed_restart.is_none() && op.resume_only {
        // The recorded no-Go result remains unchanged. Only retained objects
        // can complete an initially unsettled drain/cohort observation later.
        op.closed_restart =
            ClosedRestart::capture(&op, Retirement::NotGranted, kernel).map(Arc::new);
    }
    let proof = op.closed_restart.as_ref()?.clone();
    if proof.predecessor != op.request || !proof.available() || !proof.quiescent(kernel) {
        return None;
    }
    drop(op);
    Some((operation, proof))
}

/// Stage exactly the already-published child. Native Prepare still performs the
/// final Meta/table/old-operation proof check and consumes the predecessor once.
pub(in crate::native_act) fn stage(
    ctx: &Arc<crate::SessionCtx>,
    receipt: &Receipt,
    attempt: &renewal::Attempt,
    kernel: &dyn Kernel,
) -> Result<Prepare, &'static str> {
    if receipt.phase != Phase::Committing
        || receipt.revoked
        || receipt.accepted_turn.is_some()
        || receipt.current_attempt.as_ref() != Some(&attempt.reference)
        || attempt.root != receipt.id
    {
        return Err("continuation-intent-terminal");
    }
    let (operation, proof) = closed_predecessor(ctx, &attempt.native_predecessor, kernel)
        .ok_or("continuation-predecessor-unresolved")?;
    let original = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        op.authority
            .renewal
            .clone()
            .ok_or("continuation-not-service-owned")?
    };
    if !original.bound_to(ctx) || !receipt.same_intent(&original.receipt) {
        return Err("continuation-intent-changed");
    }
    let env = original.environment();
    if renewal::read_receipt(&env, &receipt.id).as_ref() != Some(receipt)
        || renewal::read_active_attempt(&env, receipt)?.as_ref() != Some(attempt)
    {
        return Err("continuation-journal-changed");
    }
    let Some(aterm_agent::harness::native_act::Request::Prepare(prepare)) =
        aterm_agent::harness::native_act::Request::parse(&attempt.native_prepare)
    else {
        return Err("continuation-plan");
    };
    if prepare.id != attempt.reference.id
        || prepare.plan != proof.predecessor.plan
        || prepare.host_epoch != proof.predecessor.host_epoch
        || prepare.holder != proof.predecessor.holder
        || prepare.duty != Scope::Relaunch
        || prepare.deadline_us <= proof.predecessor.deadline_us
    {
        return Err("continuation-recipe-changed");
    }
    if attempt.parent.id != proof.predecessor.id {
        // A journal entry can expire before admission, but an unknown active
        // capability cannot be skipped. The original retained witness must
        // still be unused and that parent's immutable budget must be closed.
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if table.by_id.contains_key(&attempt.parent.id) || !proof.available() {
            return Err("continuation-parent-unknown");
        }
        drop(table);
        let parent = renewal::read_attempt(&env, &receipt.id, &attempt.parent)?;
        let Some(aterm_agent::harness::native_act::Request::Prepare(parent_prepare)) =
            aterm_agent::harness::native_act::Request::parse(&parent.native_prepare)
        else {
            return Err("continuation-parent-plan");
        };
        if !kernel
            .clock()
            .now_us()
            .is_some_and(|now| now >= parent_prepare.deadline_us)
        {
            return Err("continuation-parent-budget-live");
        }
    }
    let fresh_snapshot = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    let seal = Seal {
        receipt: receipt.clone(),
        prepare: (*prepare).clone(),
        proof: original.proof.clone(),
        ctx: Arc::downgrade(ctx),
        env,
        intent: Arc::clone(&original.intent),
        record: original.record.clone(),
        continuation: Some(proof),
        fresh_snapshot: Some(fresh_snapshot),
    };
    if !seal.preflight() {
        return Err("continuation-not-ready");
    }
    // Retired seals may own watch references. Dispose after every predicate.
    let retired_seal;
    {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = table.renewal_seals.get(&prepare.id) {
            return if existing.prepare == *prepare
                && existing.receipt.same_intent(receipt)
                && Arc::ptr_eq(&existing.intent, &seal.intent)
            {
                Ok(*prepare)
            } else {
                Err("continuation-seal-conflict")
            };
        }
        if table.renewal_seals.len() >= MAX_COMPLETED {
            return Err("continuation-capacity");
        }
        table.renewal_seals.insert(prepare.id.clone(), seal);
        retired_seal = table.renewal_seals.remove(&attempt.native_predecessor);
    }
    drop(retired_seal);
    Ok(*prepare)
}
