// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A permanent journal fence precedes release of an abandoned candidate.

use super::*;
use aterm_agent::harness::native_account_wire::{
    Denial, Relation, RelationReceipt, Reply as AccountReply, Request as AccountRequest,
};

pub(super) struct FenceRecord {
    pub(super) sequence: u64,
    pub(super) recorded_ms: u64,
}

enum Refusal {
    Denied(Denial),
    Unknown,
}

impl Refusal {
    fn reply(self) -> AccountReply {
        match self {
            Self::Denied(reason) => AccountReply::Denied(reason),
            Self::Unknown => AccountReply::Unknown,
        }
    }
}

impl From<account_observation::Refusal> for Refusal {
    fn from(reason: account_observation::Refusal) -> Self {
        match reason {
            account_observation::Refusal::Authority => Self::Denied(Denial::Authority),
            account_observation::Refusal::Unknown => Self::Unknown,
        }
    }
}

/// Only this module can turn a successful exact journal transaction into the
/// witness consumed by the pending-slot latch and release operations.
pub(super) struct DurableFence<'a> {
    relation: &'a Relation,
    record: FenceRecord,
}

impl DurableFence<'_> {
    pub(super) fn relation(&self) -> &Relation {
        self.relation
    }
}

struct Witness {
    operation: Arc<Mutex<Operation>>,
    proof: Arc<ClosedRestart>,
    direct: bool,
}

/// Exclude a new, differently described owner of the same ID between the
/// in-memory check and ANF4. Existing typed preparation workers may still drain.
struct FenceFlight<'a> {
    ctx: &'a crate::SessionCtx,
    relation: &'a Relation,
}

impl<'a> FenceFlight<'a> {
    fn begin(
        ctx: &'a crate::SessionCtx,
        relation: &'a Relation,
        caller: &SupervisorInputCaller,
        kernel: &dyn Kernel,
        witness: &Witness,
    ) -> Result<Self, Refusal> {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let id = &relation.candidate().id;
        if table.account_fences.len() >= MAX_COMPLETED || table.account_fences.contains_key(id) {
            return Err(Refusal::Unknown);
        }
        if table.cancelled.contains_key(id)
            || witness
                .proof
                .pending_account_conflicts(relation.candidate())
        {
            return Err(Refusal::Denied(Denial::Conflict));
        }
        if let Some(reserved) = table.reservations.get(id)
            && reserved.as_ref() != Some(relation)
        {
            return Err(Refusal::Denied(Denial::Conflict));
        }
        if let Some(operation) = table.by_id.get(id) {
            let op = operation.lock().unwrap_or_else(|p| p.into_inner());
            if op.request != *relation.candidate()
                || op
                    .authority
                    .account
                    .as_ref()
                    .is_none_or(|account| account.relation() != relation)
                || op.preparation.recorded
            {
                return Err(Refusal::Denied(Denial::Conflict));
            }
            if !account_observation::issuer_current(&op, ctx, caller, kernel) {
                return Err(Refusal::Denied(Denial::Authority));
            }
            if !unclaimed(&op, ctx, relation, caller, kernel, &witness.proof) {
                return Err(Refusal::Unknown);
            }
        }
        table.account_fences.insert(id.clone(), relation.clone());
        Ok(Self { ctx, relation })
    }
}

impl Drop for FenceFlight<'_> {
    fn drop(&mut self) {
        let mut table = self
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if table.account_fences.get(&self.relation.candidate().id) == Some(self.relation) {
            table.account_fences.remove(&self.relation.candidate().id);
        }
    }
}

impl Witness {
    fn current(
        &self,
        ctx: &crate::SessionCtx,
        relation: &Relation,
        caller: &SupervisorInputCaller,
        kernel: &dyn Kernel,
    ) -> bool {
        let id = self
            .operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .request
            .id
            .clone();
        if !ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_id
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.operation))
        {
            return false;
        }
        let op = self.operation.lock().unwrap_or_else(|p| p.into_inner());
        if self.direct {
            account_observation::closure_witness(&op, ctx, relation.predecessor(), caller, kernel)
                .is_ok_and(|proof| Arc::ptr_eq(&proof, &self.proof))
        } else {
            inherited_witness(&op, ctx, relation, caller, kernel)
                .ok()
                .flatten()
                .is_some_and(|proof| Arc::ptr_eq(&proof, &self.proof))
        }
    }
}

fn inherited_witness(
    op: &Operation,
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> Result<Option<Arc<ClosedRestart>>, Refusal> {
    let Some(account) = op.authority.account.as_ref() else {
        return Ok(None);
    };
    let proof = account.proof();
    if proof.predecessor.id != relation.predecessor().prepare().id {
        return Ok(None);
    }
    if account.relation().predecessor() != relation.predecessor()
        || !account_observation::issuer_current(op, ctx, caller, kernel)
    {
        return Err(Refusal::Denied(Denial::Authority));
    }
    if !(op.preparation.transfers(
        relation.predecessor().prepare(),
        relation.predecessor().binding(),
    ) && proof.predecessor == *relation.predecessor().prepare()
        && proof.noexec()
        && proof.confirmed_successor(&op.request)
        && proof.quiescent(kernel))
    {
        return Err(Refusal::Unknown);
    }
    if proof.closed_phase() != relation.phase() {
        return Err(Refusal::Denied(Denial::Phase));
    }
    Ok(Some(Arc::clone(proof)))
}

fn witness(
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> Result<Witness, Refusal> {
    let (parent, children) = {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let parent = table
            .by_id
            .get(&relation.predecessor().binding().id)
            .cloned();
        let children = if parent.is_none() {
            table.by_id.values().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        (parent, children)
    };
    if let Some(operation) = parent {
        let proof = account_observation::closure_witness(
            &operation.lock().unwrap_or_else(|p| p.into_inner()),
            ctx,
            relation.predecessor(),
            caller,
            kernel,
        )?;
        if proof.closed_phase() != relation.phase() {
            return Err(Refusal::Denied(Denial::Phase));
        }
        return Ok(Witness {
            operation,
            proof,
            direct: true,
        });
    }
    for operation in children {
        let proof = inherited_witness(
            &operation.lock().unwrap_or_else(|p| p.into_inner()),
            ctx,
            relation,
            caller,
            kernel,
        )?;
        if let Some(proof) = proof {
            return Ok(Witness {
                operation,
                proof,
                direct: false,
            });
        }
    }
    Err(Refusal::Unknown)
}

fn unclaimed(
    op: &Operation,
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    proof: &Arc<ClosedRestart>,
) -> bool {
    account_observation::retained_claim(op, ctx, relation, caller, kernel)
        && op
            .authority
            .account
            .as_ref()
            .is_some_and(|account| Arc::ptr_eq(account.proof(), proof))
        && !op.preparation.recorded
        && op.phase == Phase::Queued
        && op.grant_record == GrantRecord::None
        && account_claim::before_dispatch(op)
}

/// Freeze before dropping custody. Retry is the only owner of cleanup after an
/// interrupted reply; it replays the same permanent receipt, never a missing row.
fn settle(
    ctx: &crate::SessionCtx,
    fence: &DurableFence<'_>,
    witness: &Witness,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> bool {
    let relation = fence.relation;
    let id = &relation.candidate().id;
    let (candidate, custody) = {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let candidate = table.by_id.get(id).cloned();
        let mut op = candidate
            .as_ref()
            .map(|op| op.lock().unwrap_or_else(|p| p.into_inner()));
        if op
            .as_ref()
            .is_some_and(|op| !unclaimed(op, ctx, relation, caller, kernel, &witness.proof))
        {
            return false;
        }
        if !witness.proof.fence_account_successor(
            fence,
            table
                .reservations
                .get(id)
                .is_some_and(|reserved| reserved.as_ref() == Some(relation))
                || op.is_some(),
        ) {
            return false;
        }
        let custody = op.as_mut().and_then(|op| {
            if !op.preparation.idle() {
                return None;
            }
            if let Some(custody) = &op.custody {
                op.completion_drain = Some(custody.drain());
            }
            op.channel = None;
            op.custody.take()
        });
        drop(op);
        (candidate, custody)
    };
    drop(custody);
    let removed = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if table
            .reservations
            .get(id)
            .is_some_and(|reserved| reserved.as_ref() == Some(relation))
        {
            return false;
        }
        if let Some(current) = table.by_id.get(id) {
            if !candidate
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, current))
            {
                return false;
            }
            let op = current.lock().unwrap_or_else(|p| p.into_inner());
            if !unclaimed(&op, ctx, relation, caller, kernel, &witness.proof)
                || !op.preparation.idle()
                || op.custody.is_some()
                || op
                    .completion_drain
                    .as_ref()
                    .is_some_and(|drain| !drain.is_drained())
            {
                return false;
            }
        }
        witness.proof.release_fenced_account(fence);
        table.account_lineages.cancel(
            &AccountSlot::Successor(relation.predecessor().binding().id.clone()),
            id,
        );
        table.completed.retain(|completed| completed != id);
        table.by_id.remove(id)
    };
    drop(removed);
    true
}

/// Fencing proves durable non-execution, then releases the exact pending slot.
/// It does not renew execution authority or require the old account to be usable.
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "Fence remains internal until client CAS and the full protocol are complete."
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountFence",
        action = "Settle",
        project = "native_act::tests::account_continuation_tests::fence_transaction::fence_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "Reserve",
        reason = "The fixture owns a real preparation reservation before the transaction"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "Insert",
        reason = "The existing Claim owner inserts a queued operation"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "CancelReservation",
        reason = "Dropping a preparation slot releases its exact reservation"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "FinishWriter",
        reason = "The preparation owner releases its token after unlocked journal work"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "HoldDrain",
        reason = "An independently retained native custody marker remains alive"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "Drain",
        reason = "Dropping the last native custody marker settles the drain"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "ClaimWrite",
        reason = "The exclusive journal transaction publishes ANS4 before Fence can win"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "FenceWrite",
        reason = "The fixture commits ANF4 with the production journal writer"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "LateConfirm",
        reason = "The mutant models an invalid confirmation after the pending fence latch"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "ReserveNext",
        reason = "A separate exact candidate reserves the predecessor after completed cleanup"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFence",
        action = "NextClaim",
        reason = "A later candidate records its own claim without overwriting the earlier permanent fence"
    )
)]
pub(super) fn fence(
    ctx: &crate::SessionCtx,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> AccountReply {
    let Ok(_candidate_flight) =
        ReceiptFlight::begin(ctx, &Request::prepare(relation.candidate().clone()))
    else {
        return AccountReply::Unknown;
    };
    let Ok(_parent_flight) = ReceiptFlight::begin(
        ctx,
        &Request::Query(relation.predecessor().binding().clone()),
    ) else {
        return AccountReply::Unknown;
    };
    let Some(_handoff) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return AccountReply::Unknown;
    };
    let witness = match witness(ctx, relation, caller, kernel) {
        Ok(witness) => witness,
        Err(reason) => return reason.reply(),
    };
    let binding = witness
        .operation
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .binding
        .clone();
    let Ok(_witness_flight) = ReceiptFlight::begin(ctx, &Request::Query(binding)) else {
        return AccountReply::Unknown;
    };
    if !witness.current(ctx, relation, caller, kernel) {
        return AccountReply::Unknown;
    }
    if witness.proof.confirmed_successor(relation.candidate()) {
        return AccountReply::Denied(Denial::Conflict);
    }
    let _fence_flight = match FenceFlight::begin(ctx, relation, caller, kernel, &witness) {
        Ok(flight) => flight,
        Err(reason) => return reason.reply(),
    };
    if witness.direct {
        let Some(record) = kernel.record_account_closure(
            relation.predecessor().prepare(),
            relation.predecessor().binding(),
            relation.phase(),
        ) else {
            return AccountReply::Unknown;
        };
        if record.phase != relation.phase() {
            return AccountReply::Unknown;
        }
    }
    let Some(record) = kernel.fence_account_claim(relation) else {
        if witness.proof.confirmed_successor(relation.candidate()) {
            return AccountReply::Denied(Denial::Conflict);
        }
        return AccountReply::Unknown;
    };
    let fence = DurableFence { relation, record };
    if !settle(ctx, &fence, &witness, caller, kernel)
        || !witness.current(ctx, relation, caller, kernel)
    {
        return AccountReply::Unknown;
    }
    let reply = AccountReply::Fenced(RelationReceipt::for_relation(
        relation,
        fence.record.sequence,
        fence.record.recorded_ms,
    ));
    if reply.acknowledges(&AccountRequest::Fence(relation.clone())) {
        reply
    } else {
        AccountReply::Unknown
    }
}
