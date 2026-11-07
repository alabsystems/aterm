// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary account authority retains a closed predecessor, never a renewal seal.
//! The private entry point is intentionally not exposed by the control protocol:
//! activation also needs bounded lineage retention and durable client slots.

use super::*;
use aterm_agent::harness::native_account_wire::Relation;

#[cfg(unix)]
pub(super) fn recorded_state(
    outcome: aterm_agent::harness::native_receipts::AccountOutcome,
) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
    use aterm_agent::harness::native_account_wire::ChildState;
    use aterm_agent::harness::native_receipts::{AccountOutcome, ClaimedState};
    Some(match outcome {
        AccountOutcome::Fenced => return None,
        AccountOutcome::Claimed(state) => match state {
            ClaimedState::Prepared => ChildState::Prepared,
            ClaimedState::MayExecute => ChildState::MayExecute,
            ClaimedState::NoExec { phase } => ChildState::NoExec(phase),
            ClaimedState::Sealed { may_execute } => ChildState::Sealed { may_execute },
        },
    })
}

#[derive(Clone, Debug)]
pub(super) struct AccountContinuation {
    relation: Relation,
    proof: Arc<ClosedRestart>,
    actor: Process,
    conversation: Conversation,
}

impl AccountContinuation {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Account authority capture is staged behind the private entry until durable client slots and lineage retention are complete."
        )
    )]
    fn capture(
        ctx: &crate::SessionCtx,
        relation: &Relation,
        caller: &SupervisorInputCaller,
        kernel: &dyn Kernel,
    ) -> Result<Self, &'static str> {
        let parent = relation.predecessor();
        let binding = parent.binding();
        let candidate = relation.candidate();
        let previous = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_id
            .get(&binding.id)
            .cloned()
            .ok_or(UNKNOWN)?;
        let op = previous.lock().unwrap_or_else(|p| p.into_inner());
        if op.binding != *binding
            || op.request != *parent.prepare()
            || !op.authority.builtin
            || op.authority.renewal.is_some()
            || op.grant_record != GrantRecord::None
            || !same_actor(&op, caller, kernel)
            || !matches!(
                op.request.plan,
                NativePlan::Restart {
                    observation: None,
                    cold: None,
                    ..
                }
            )
        {
            return Err(DENIED);
        }
        if op.phase != Phase::Completed(Decision::Retired(Retirement::NotGranted)) {
            return Err(if matches!(op.phase, Phase::Completed(_)) {
                DENIED
            } else {
                UNKNOWN
            });
        }
        let proof = op.closed_restart.as_ref().ok_or(UNKNOWN)?;
        if !op.preparation.can_publish() || !proof.quiescent(kernel) {
            return Err(UNKNOWN);
        }
        if !proof.noexec() || (!proof.available() && !proof.claimed_by(candidate)) {
            return Err(if proof.pending_account_by(candidate) {
                UNKNOWN
            } else {
                DENIED
            });
        }
        let account = Self {
            relation: relation.clone(),
            proof: Arc::clone(proof),
            actor: op.authority.actor.clone().ok_or(DENIED)?,
            conversation: op.authority.conversation.clone().ok_or(DENIED)?,
        };
        drop(op);
        if account.current(ctx, candidate, caller, kernel) {
            Ok(account)
        } else {
            Err(DENIED)
        }
    }

    pub(super) fn admit(
        &self,
        kernel: &dyn Kernel,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        let NativePlan::Restart {
            forward, rollback, ..
        } = &self.relation.candidate().plan
        else {
            return Err(());
        };
        let command = match self.relation.phase() {
            ImagePhase::Forward => forward.as_ref(),
            ImagePhase::Rollback => rollback.as_deref().ok_or(())?,
        };
        kernel.account_admission(command.account_intent.as_deref().ok_or(())?)
    }

    pub(super) fn proof(&self) -> &Arc<ClosedRestart> {
        &self.proof
    }

    pub(super) fn relation(&self) -> &Relation {
        &self.relation
    }

    pub(super) fn same_relation(&self, other: &Self) -> bool {
        self.relation == other.relation && Arc::ptr_eq(&self.proof, &other.proof)
    }

    /// This is additional authority, never a replacement for current native
    /// policy, deadline, constructor, account admission or preparation checks.
    pub(super) fn current(
        &self,
        ctx: &crate::SessionCtx,
        candidate: &Prepare,
        caller: &SupervisorInputCaller,
        kernel: &dyn Kernel,
    ) -> bool {
        self.relation.candidate() == candidate
            && self.proof.predecessor == *self.relation.predecessor().prepare()
            && candidate.host_epoch == self.proof.predecessor.host_epoch
            && host_epoch() == Some(candidate.host_epoch.as_str())
            && self.proof.closed_phase() == self.relation.phase()
            && self
                .proof
                .predecessor
                .account_successor_recipe(candidate, self.relation.phase())
            && self.proof.noexec()
            && self.proof.quiescent(kernel)
            && self.conversation.current(ctx)
            && caller.principal == crate::control::Principal::Harness
            && caller.peer == Some(self.actor.pid)
            && kernel.process(self.actor.pid).is_some_and(|now| {
                own_actor_matches(&now, &self.actor) && now.image == self.actor.image
            })
    }
}

/// Exercise the entire host admission path without publishing a partial wire
/// contract. The future transaction must supply the same predecessor and phase
/// on every replay; an ordinary Prepare cannot stand in for this relation.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "The account entry is intentionally internal until durable client slots and lineage retention are complete."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountContinuation",
        action = "Check",
        project = "native_act::tests::account_continuation_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Exited",
        reason = "An input fixture changes the predecessor terminal classification."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Grant",
        reason = "An input fixture supplies retained grant evidence."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Capability",
        reason = "An input fixture supplies a different predecessor capability."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Actor",
        reason = "An input fixture supplies a different caller identity."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Conversation",
        reason = "The original logical conversation independently becomes stale."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Recipe",
        reason = "An input fixture changes the immutable command recipe."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Phase",
        reason = "An input fixture supplies a phase different from the retained closure."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Policy",
        reason = "A new supervisor claim takes the builtin Relaunch duty."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Cohort",
        reason = "An input fixture introduces a live member of the original process cohort."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "AccountPool",
        reason = "The independently locked account store refuses the selected generation or account policy."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountContinuation",
        action = "Predecessor",
        reason = "The caller supplies a structurally valid relation for a different original preparation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClaimPublication",
        action = "ReserveA",
        project = "native_act::tests::account_continuation_tests::claim_model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClaimPublication",
        action = "ReserveB",
        project = "native_act::tests::account_continuation_tests::claim_model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClaimPublication",
        action = "Persist",
        project = "native_act::tests::account_continuation_tests::claim_model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClaimPublication",
        action = "Confirm",
        project = "native_act::tests::account_continuation_tests::claim_model::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountClaimPublication",
        action = "CloseOnDisk",
        reason = "Another publisher advances the journal while an admission acknowledgement is lost"
    )
)]
pub(super) fn prepare_account(
    ctx: &crate::SessionCtx,
    master: i32,
    relation: Relation,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    serve_with_publication(
        ctx,
        Request::prepare(relation.candidate().clone()),
        kernel,
        false,
        |_| {
            let _parent_flight = match ReceiptFlight::begin(
                ctx,
                &Request::Query(relation.predecessor().binding().clone()),
            ) {
                Ok(flight) => flight,
                Err(()) => return UNKNOWN.into(),
            };
            prepare_account_relation(ctx, master, relation, caller, kernel)
        },
    )
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Account relation admission is staged behind the private entry until the transaction is ready for protocol activation."
    )
)]
fn prepare_account_relation(
    ctx: &crate::SessionCtx,
    master: i32,
    relation: Relation,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    let candidate = relation.candidate();
    // A lost acknowledgement may have consumed the predecessor already. Only
    // the exact retained child relation can recover it, never a fresh capture.
    let previous = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&candidate.id)
        .cloned();
    let account = if let Some(previous) = previous {
        let op = previous.lock().unwrap_or_else(|p| p.into_inner());
        let Some(account) = &op.authority.account else {
            return DENIED.into();
        };
        if account.relation != relation {
            return DENIED.into();
        }
        let account = account.clone();
        drop(op);
        account
    } else {
        let account = match capture_or_replay(ctx, &relation, || {
            AccountContinuation::capture(ctx, &relation, &caller, kernel)
        }) {
            Ok(account) => account,
            Err(refusal) => return refusal.into(),
        };
        if account.proof().claimed_by(candidate)
            && !ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .by_id
                .contains_key(&candidate.id)
        {
            return UNKNOWN.into();
        }
        account
    };
    prepare_inner(
        ctx,
        master,
        candidate.clone(),
        caller,
        kernel,
        Some(account),
    )
}

/// A concurrent exact publisher may consume or retire the predecessor between
/// the first child lookup and capture. Recheck its retained relation, never
/// turn that race into a fresh capability or a contradictory denial.
pub(super) fn capture_or_replay(
    ctx: &crate::SessionCtx,
    relation: &Relation,
    capture: impl FnOnce() -> Result<AccountContinuation, &'static str>,
) -> Result<AccountContinuation, &'static str> {
    match capture() {
        Ok(account) => Ok(account),
        Err(refusal) => {
            let child = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .by_id
                .get(&relation.candidate().id)
                .cloned()
                .ok_or(refusal)?;
            let child = child.lock().unwrap_or_else(|p| p.into_inner());
            child
                .authority
                .account
                .as_ref()
                .filter(|account| account.relation == *relation)
                .cloned()
                .ok_or(DENIED)
        }
    }
}
