// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed account admission, staged until the complete client transaction exists.

use super::*;
use aterm_agent::harness::native_account_wire::{ChildState, Relation, Reply as AccountReply};

fn settled(op: &Operation, relation: &Relation) -> bool {
    op.preparation.settled_account(
        relation.predecessor().prepare(),
        relation.predecessor().binding(),
    ) && op
        .authority
        .account
        .as_ref()
        .is_some_and(|account| account.proof().confirmed_successor(relation.candidate()))
}

// Once dispatch has begun, its lifecycle owner must settle the operation.
// An ungranted helper is still a live participant, not a NoExec witness.
pub(super) fn before_dispatch(op: &Operation) -> bool {
    (!op.signal_sent || op.signal_accepted == Some(false))
        && !op.resume_sent
        && op.resume_attempt.is_none()
        && op.children.iter().all(Option::is_none)
}

fn drained(op: &Operation) -> bool {
    op.completion_drain
        .as_ref()
        .is_none_or(NativeDrain::is_drained)
}

/// Reuse the existing admission owner, then acknowledge only durable settled
/// state. Neither a native reply string nor journal Prepared grants execution.
#[cfg_attr(
    not(all(test, unix)),
    allow(
        dead_code,
        reason = "Typed Claim awaits the complete host/client transaction before protocol activation."
    )
)]
pub(super) fn claim(
    ctx: &crate::SessionCtx,
    master: i32,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> AccountReply {
    let request = Request::prepare(relation.candidate().clone());
    let Ok(_flight) = ReceiptFlight::begin(ctx, &request) else {
        return AccountReply::Unknown;
    };
    let Some(_handoff) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return AccountReply::Unknown;
    };
    let _admission = account_continuation::prepare_account(
        ctx,
        master,
        relation.clone(),
        caller.clone(),
        kernel,
    );
    let _settlement = serve_with_publication(ctx, request, kernel, false, |_| {
        settle_invalid(ctx, master, relation, caller, kernel)
    });
    account_observation::recover_claim_checked(ctx, relation, caller, kernel, |op, state| {
        ready(ctx, master, op, relation, caller, kernel, state)
    })
}

fn settle_invalid(
    ctx: &crate::SessionCtx,
    master: i32,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&relation.candidate().id)
        .cloned();
    let Some(operation) = operation else {
        return UNKNOWN.into();
    };
    let account = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !account_observation::retained_claim(&op, ctx, relation, caller, kernel) {
            return DENIED.into();
        }
        if !settled(&op, relation)
            || op.phase != Phase::Started
            || op.grant_record != GrantRecord::None
            || !before_dispatch(&op)
        {
            return UNKNOWN.into();
        }
        op.authority
            .account
            .clone()
            .expect("matched account relation")
    };
    let guard = account.admit(kernel);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if !account_observation::retained_claim(&op, ctx, relation, caller, kernel) {
        return DENIED.into();
    }
    if !settled(&op, relation)
        || op.phase != Phase::Started
        || op.grant_record != GrantRecord::None
        || !before_dispatch(&op)
        || preparation::authority_current(ctx, master, &op, &meta, caller, kernel, guard.is_ok())
    {
        return UNKNOWN.into();
    }
    let completion = op.seal_without_effect(kernel);
    drop(op);
    drop(meta);
    drop(guard);
    if completion.is_some() {
        kernel.completion_sealed();
    }
    finish(ctx, &operation, completion)
}

#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountClaimReadiness",
        action = "Check",
        project = "native_act::tests::account_continuation_tests::transaction::readiness_transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "Dispatch",
        reason = "The fixture supplies dispatch begun by the existing lifecycle owner"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "RecoverTerminal",
        reason = "The fixture recovers a retained durable terminal classification through admission"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "PendingDrain",
        reason = "A retained native custody marker supplies independently outstanding completion work"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "Queue",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "CloseNoExec",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "CloseUnknown",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "ReadNoExec",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "ReadMay",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "ReadSealed",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "WrongIdentity",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "Unpublished",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "WriterActive",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "Unconfirmed",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "Revoke",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "CohortUnknown",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "WrongPhase",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimReadiness",
        action = "Grant",
        reason = "The fixture supplies independent retained state or live facts before the final acknowledgement predicate"
    )
)]
pub(super) fn ready(
    ctx: &crate::SessionCtx,
    master: i32,
    operation: &Arc<Mutex<Operation>>,
    relation: &Relation,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    state: ChildState,
) -> bool {
    let account = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !account_observation::retained_claim(&op, ctx, relation, caller, kernel)
            || !settled(&op, relation)
        {
            return false;
        }
        op.authority
            .account
            .clone()
            .expect("matched account relation")
    };
    // Account-file reads precede both Meta and the operation predicate. The
    // guard is needed only for a candidate that is still eligible for Resume.
    let guard = (state == ChildState::Prepared).then(|| account.admit(kernel));
    let meta =
        (state == ChildState::Prepared).then(|| ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if !account_observation::retained_claim(&op, ctx, relation, caller, kernel)
        || !settled(&op, relation)
    {
        return false;
    }
    match state {
        ChildState::Prepared => {
            op.phase == Phase::Started
                && op.grant_record == GrantRecord::None
                && before_dispatch(&op)
                && preparation::authority_current(
                    ctx,
                    master,
                    &op,
                    meta.as_ref().expect("prepared metadata"),
                    caller,
                    kernel,
                    guard.as_ref().is_some_and(Result::is_ok),
                )
        }
        ChildState::NoExec(phase) => {
            op.phase == Phase::Completed(Decision::Retired(Retirement::NotGranted))
                && op.grant_record == GrantRecord::None
                && op.resume_phase.unwrap_or(ImagePhase::Forward) == phase
                && drained(&op)
                && op.closed_restart.as_ref().is_some_and(|proof| {
                    proof.noexec()
                        && proof.closed_phase() == phase
                        && proof.predecessor == op.request
                        && proof.quiescent(kernel)
                })
        }
        ChildState::MayExecute => {
            op.phase != Phase::Queued
                && op.phase != Phase::Completed(Decision::Retired(Retirement::NotGranted))
                && op
                    .closed_restart
                    .as_ref()
                    .is_none_or(|proof| !proof.noexec())
                && (op.grant_record != GrantRecord::None
                    || (op.phase == Phase::Completed(Decision::Unknown)
                        && op.preparation.terminal_already_recorded()))
        }
        ChildState::Sealed { .. } => {
            matches!(op.phase, Phase::Completed(_))
                && op.phase != Phase::Completed(Decision::Retired(Retirement::NotGranted))
                && op
                    .closed_restart
                    .as_ref()
                    .is_none_or(|proof| !proof.noexec())
                && drained(&op)
        }
    }
}
