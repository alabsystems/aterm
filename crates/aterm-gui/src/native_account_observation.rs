// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed issuer observations, staged without a control verb or client dispatch.
//! Retained capabilities and live identity checks precede unlocked journal I/O.

use super::*;
use aterm_agent::harness::native_account_wire::{
    ChildState, ClosureReceipt, Denial, Predecessor, Relation, RelationReceipt,
    Reply as AccountReply, Request as AccountRequest,
};

#[derive(Clone, Copy)]
pub(super) struct ClosureRecord {
    pub(super) phase: ImagePhase,
    pub(super) sequence: u64,
    pub(super) recorded_ms: u64,
}

#[derive(Clone, Copy)]
pub(super) struct ClaimRecord {
    pub(super) state: ChildState,
    pub(super) sequence: u64,
    pub(super) recorded_ms: u64,
}

pub(super) enum Refusal {
    Authority,
    Unknown,
}

impl Refusal {
    pub(super) fn reply(self) -> AccountReply {
        match self {
            Self::Authority => AccountReply::Denied(Denial::Authority),
            Self::Unknown => AccountReply::Unknown,
        }
    }
}

pub(super) fn issuer_current(
    op: &Operation,
    ctx: &crate::SessionCtx,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> bool {
    op.authority.builtin
        && op.authority.renewal.is_none()
        && caller.principal == crate::control::Principal::Harness
        && same_actor(op, caller, kernel)
        && op
            .authority
            .conversation
            .as_ref()
            .is_some_and(|conversation| conversation.current(ctx))
        && host_epoch() == Some(op.request.host_epoch.as_str())
}

pub(super) fn closure_witness(
    op: &Operation,
    ctx: &crate::SessionCtx,
    parent: &Predecessor,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> Result<Arc<ClosedRestart>, Refusal> {
    if op.request != *parent.prepare()
        || op.binding != *parent.binding()
        || !issuer_current(op, ctx, caller, kernel)
    {
        return Err(Refusal::Authority);
    }
    if op.phase != Phase::Completed(Decision::Retired(Retirement::NotGranted))
        || !op.preparation.can_publish()
        || op.grant_record != GrantRecord::None
        || op
            .completion_drain
            .as_ref()
            .is_some_and(|drain| !drain.is_drained())
    {
        return Err(Refusal::Unknown);
    }
    let proof = op.closed_restart.as_ref().ok_or(Refusal::Unknown)?;
    if proof.predecessor != *parent.prepare()
        || !proof.noexec()
        || op.resume_phase.unwrap_or(ImagePhase::Forward) != proof.closed_phase()
        || !parent.supports_phase(proof.closed_phase())
        || !proof.quiescent(kernel)
    {
        return Err(Refusal::Unknown);
    }
    Ok(Arc::clone(proof))
}

/// Acknowledges the exact live host's closed phase. The expired admission
/// deadline and a revoked account generation cannot erase an existing closure.
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "Typed account observations await protocol activation; their executable conformance uses Unix journal custody."
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountHostObservation",
        action = "Check",
        project = "native_act::tests::account_continuation_tests::observation::transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountHostObservation",
        action = "UseClaim",
        reason = "The fixture selects the requested observation kind"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountHostObservation",
        action = "NextCase",
        reason = "The fixture selects an independent live or journal premise"
    )
)]
pub(super) fn closure(
    ctx: &crate::SessionCtx,
    parent: &Predecessor,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> AccountReply {
    let Ok(_flight) = ReceiptFlight::begin(ctx, &Request::Query(parent.binding().clone())) else {
        return AccountReply::Unknown;
    };
    let Some(operation) = lookup(ctx, parent.binding()) else {
        return AccountReply::Unknown;
    };
    let proof = match closure_witness(
        &operation.lock().unwrap_or_else(|p| p.into_inner()),
        ctx,
        parent,
        caller,
        kernel,
    ) {
        Ok(proof) => proof,
        Err(refusal) => return refusal.reply(),
    };
    let Some(_handoff) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return AccountReply::Unknown;
    };
    let phase = proof.closed_phase();
    let Some(record) = kernel.record_account_closure(parent.prepare(), parent.binding(), phase)
    else {
        return AccountReply::Unknown;
    };
    let checked = closure_witness(
        &operation.lock().unwrap_or_else(|p| p.into_inner()),
        ctx,
        parent,
        caller,
        kernel,
    );
    if record.phase != phase || !checked.is_ok_and(|current| Arc::ptr_eq(&current, &proof)) {
        return AccountReply::Unknown;
    }
    let reply = AccountReply::Closure {
        receipt: ClosureReceipt::for_predecessor(parent, record.sequence, record.recorded_ms),
        phase,
        successor: proof.successor_id(),
    };
    if reply.acknowledges(&AccountRequest::Closure(parent.clone())) {
        reply
    } else {
        AccountReply::Unknown
    }
}

pub(super) fn retained_claim(
    op: &Operation,
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> bool {
    op.request == *relation.candidate()
        && op.binding.id == relation.candidate().id
        && issuer_current(op, ctx, caller, kernel)
        && op.authority.account.as_ref().is_some_and(|account| {
            account.relation() == relation
                && account.proof().predecessor == *relation.predecessor().prepare()
        })
}

/// Recover only an existing exact claim. No request, candidate or capability is
/// allocated here; a missing journal result remains Unknown, never a fence.
/// Prepared describes journal state, not live admission. The eventual Claim
/// dispatcher must still finish admission or retirement before client dispatch.
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "Typed account observations await protocol activation; their executable conformance uses Unix journal custody."
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountHostObservation",
        action = "Check",
        project = "native_act::tests::account_continuation_tests::observation::transition"
    )
)]
pub(super) fn recover_claim(
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> AccountReply {
    recover_claim_checked(ctx, relation, caller, kernel, |_, _| true)
}

/// The caller's final-use predicate runs after journal I/O without native or
/// metadata locks held. Identity is rechecked after that predicate too.
pub(super) fn recover_claim_checked(
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    ready: impl FnOnce(&Arc<Mutex<Operation>>, ChildState) -> bool,
) -> AccountReply {
    let Ok(_flight) = ReceiptFlight::begin(ctx, &Request::prepare(relation.candidate().clone()))
    else {
        return AccountReply::Unknown;
    };
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&relation.candidate().id)
        .cloned();
    let Some(operation) = operation else {
        return AccountReply::Unknown;
    };
    let binding = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !retained_claim(&op, ctx, relation, caller, kernel) {
            return AccountReply::Denied(Denial::Authority);
        }
        op.binding.clone()
    };
    let Some(_handoff) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return AccountReply::Unknown;
    };
    let Some(record) = kernel.recover_account_observation(relation, &binding) else {
        return AccountReply::Unknown;
    };
    if !ready(&operation, record.state) {
        return AccountReply::Unknown;
    }
    {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op.binding != binding || !retained_claim(&op, ctx, relation, caller, kernel) {
            return AccountReply::Unknown;
        }
    }
    let reply = AccountReply::Claimed {
        receipt: RelationReceipt::for_relation(relation, record.sequence, record.recorded_ms),
        binding,
        state: record.state,
    };
    if reply.acknowledges(&AccountRequest::Claim(relation.clone())) {
        reply
    } else {
        AccountReply::Unknown
    }
}
