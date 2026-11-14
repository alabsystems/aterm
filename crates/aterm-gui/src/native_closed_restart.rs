// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained closed-attempt evidence. This type grants no mutation: a later
//! sealed service must separately admit a new operation against current policy.

use super::*;

#[derive(Debug, Default)]
struct Succession {
    retired: bool,
    pending_account: Option<PendingAccount>,
    successor: Option<Prepare>,
}

#[derive(Debug)]
enum PendingAccount {
    Reserved(Prepare),
    Fenced(Prepare),
}

impl PendingAccount {
    fn request(&self) -> &Prepare {
        match self {
            Self::Reserved(request) | Self::Fenced(request) => request,
        }
    }

    fn reserved_for(&self, request: &Prepare) -> bool {
        matches!(self, Self::Reserved(pending) if pending == request)
    }
}

#[derive(Debug)]
pub(super) struct ClosedRestart {
    pub(super) predecessor: Prepare,
    source: Arc<SharedDeath>,
    children: Vec<(Process, Arc<SharedDeath>)>,
    drain: Option<NativeDrain>,
    resume_phase: ImagePhase,
    service_owned: bool,
    succession: std::sync::Mutex<Succession>,
}

impl ClosedRestart {
    #[cfg(test)]
    pub(super) fn account_state(&self) -> (Option<Prepare>, Option<Prepare>, bool) {
        let state = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        (
            state
                .pending_account
                .as_ref()
                .map(|pending| pending.request().clone()),
            state.successor.clone(),
            state.retired,
        )
    }
    #[cfg(test)]
    pub(super) fn projected_state(&self) -> (bool, Option<Prepare>, bool) {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        (
            succession.retired,
            succession.successor.clone(),
            self.children.is_empty(),
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "LoseWatch",
            reason = "An unavailable retained kernel object is an injected external fact"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "SourceDies",
            reason = "The injected kernel witness observes source death"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "GroupDrains",
            reason = "The injected kernel observes cohort drainage"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "InputSettles",
            reason = "The native input producer settles its owned work"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "IssueExec",
            reason = "Native Execute records the independent child grant"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "ChildDies",
            reason = "The retained child watch reports death"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedRestartEvidence",
            action = "ChildGroupDrains",
            reason = "The kernel observes drainage of the issued child cohort"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedRestartEvidence",
            action = "Capture",
            project = "native_act::tests::closed_restart_model::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedRestartEvidence",
            action = "CaptureExited",
            project = "native_act::tests::closed_restart_model::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ContinuationPhase",
            action = "StartRollback",
            reason = "The existing Resume protocol establishes the active rollback phase"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ContinuationPhase",
            action = "Capture",
            project = "native_act::tests::continuation_phase::transition"
        )
    )]
    pub(super) fn capture(op: &Operation, reason: Retirement, kernel: &dyn Kernel) -> Option<Self> {
        if matches!(
            op.grant_record,
            super::GrantRecord::Writing(_) | super::GrantRecord::Uncertain(_)
        ) || (reason == Retirement::NotGranted && op.grant_record != super::GrantRecord::None)
        {
            return None;
        }
        let NativePlan::Restart { process, .. } = &op.request.plan else {
            return None;
        };
        let source = op.source_death.as_ref()?;
        if !source.exited()
            || kernel.group_gone(process.group) != Some(true)
            || op
                .resume_attempt
                .as_ref()
                .is_some_and(|attempt| !attempt.settled())
        {
            return None;
        }
        let mut children = Vec::new();
        for child in op
            .children
            .iter()
            .flatten()
            .filter(|child| child.execute_sent)
        {
            if !child.death.exited() || kernel.group_gone(child.process.group) != Some(true) {
                return None;
            }
            children.push((child.process.clone(), Arc::clone(&child.death)));
        }
        if (reason == Retirement::NotGranted) != children.is_empty() {
            return None;
        }
        Some(Self {
            predecessor: op.request.clone(),
            source: Arc::clone(source),
            children,
            drain: op
                .custody
                .as_ref()
                .map(|custody| custody.drain())
                .or_else(|| op.completion_drain.clone()),
            resume_phase: op.resume_phase.unwrap_or(ImagePhase::Forward),
            service_owned: op
                .authority
                .renewal
                .as_ref()
                .is_some_and(|seal| seal.intent_active()),
            succession: std::sync::Mutex::new(Succession::default()),
        })
    }

    pub(super) fn pinned(&self) -> bool {
        self.service_owned && self.available()
    }

    pub(super) fn source_watch(&self) -> Arc<SharedDeath> {
        Arc::clone(&self.source)
    }

    /// Carry the rollback latch even if the next operation retires before its
    /// first Resume. A forward continuation keeps ordinary first-Resume checks.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ContinuationPhase",
            action = "Admit",
            project = "native_act::tests::continuation_phase::transition"
        )
    )]
    pub(super) fn initial_resume_phase(&self) -> Option<ImagePhase> {
        (self.resume_phase == ImagePhase::Rollback).then_some(ImagePhase::Rollback)
    }

    /// The host's frozen phase at closure, independently of a client's mirror.
    pub(super) fn closed_phase(&self) -> ImagePhase {
        self.resume_phase
    }

    pub(super) fn noexec(&self) -> bool {
        // Capture only admits zero children for a positively NotGranted closure.
        self.children.is_empty()
    }

    pub(super) fn available(&self) -> bool {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        !succession.retired
            && succession.pending_account.is_none()
            && succession.successor.is_none()
    }

    pub(super) fn claimed_by(&self, request: &Prepare) -> bool {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        !succession.retired && succession.successor.as_ref() == Some(request)
    }

    /// Historical ownership survives retirement; it grants no live authority.
    pub(super) fn confirmed_successor(&self, request: &Prepare) -> bool {
        self.succession
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .successor
            .as_ref()
            == Some(request)
    }

    /// An advisory retained successor identity, never an absence or grant proof.
    pub(super) fn successor_id(&self) -> Option<String> {
        self.succession
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .successor
            .as_ref()
            .map(|request| request.id.clone())
    }

    pub(super) fn pending_account_by(&self, request: &Prepare) -> bool {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        !succession.retired
            && succession
                .pending_account
                .as_ref()
                .is_some_and(|pending| pending.reserved_for(request))
    }

    /// Historical recovery survives retirement, unlike admission. A fenced or
    /// released reservation cannot start another writer through a stale Arc.
    pub(super) fn recoverable_account(&self, request: &Prepare) -> bool {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        succession.successor.as_ref() == Some(request)
            || succession
                .pending_account
                .as_ref()
                .is_some_and(|pending| pending.reserved_for(request))
    }

    pub(super) fn pending_account_conflicts(&self, request: &Prepare) -> bool {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        succession.pending_account.as_ref().is_some_and(|pending| {
            pending.request().id == request.id && pending.request() != request
        })
    }

    /// An unresolved candidate owns the frontier even after its preparation
    /// slot unwinds or live authority retires. Only exact settlement releases it.
    pub(super) fn keeps_account_frontier(&self) -> bool {
        let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        succession.pending_account.is_some()
            || (!succession.retired && succession.successor.is_none())
    }

    /// Called only at the matched terminal operation's admission boundary.
    /// Exact replays use the existing child operation; absence never remints it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedRestartEvidence",
            action = "ClaimA",
            project = "native_act::tests::closed_restart_model::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedRestartEvidence",
            action = "ClaimB",
            project = "native_act::tests::closed_restart_model::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedRestartEvidence",
            action = "Retry",
            project = "native_act::tests::closed_restart_model::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimPublication",
            action = "Rival",
            project = "native_act::tests::account_continuation_tests::claim_model::transition"
        )
    )]
    pub(super) fn claim_successor(&self, request: &Prepare) -> bool {
        // Retirement can come from the service after preparation has released
        // the predecessor's operation lock. Keep its decision and this claim
        // in one critical section, including the availability check.
        let mut succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        if succession.retired
            || succession.pending_account.is_some()
            || succession.successor.is_some()
        {
            return false;
        }
        succession.successor = Some(request.clone());
        true
    }

    /// Reserve the exact account candidate before journal I/O. This records no
    /// acknowledged claim; a lost acknowledgement leaves the reservation unresolved.
    pub(super) fn reserve_account_successor(&self, request: &Prepare) -> bool {
        let mut succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        if succession.retired || succession.successor.is_some() {
            return false;
        }
        match &succession.pending_account {
            Some(pending) => pending.reserved_for(request),
            None => {
                succession.pending_account = Some(PendingAccount::Reserved(request.clone()));
                true
            }
        }
    }

    /// Retain the journal's acknowledged claim even if retirement raced with
    /// the write. `claimed_by` still refuses live authority after retirement.
    pub(super) fn confirm_account_successor(&self, request: &Prepare) -> bool {
        let mut succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        if succession.successor.as_ref() == Some(request) {
            return true;
        }
        if succession.successor.is_some()
            || !succession
                .pending_account
                .as_ref()
                .is_some_and(|pending| pending.reserved_for(request))
        {
            return false;
        }
        succession.successor = succession
            .pending_account
            .take()
            .map(|pending| pending.request().clone());
        true
    }

    /// A durable journal fence stops the exact pending candidate while its
    /// original worker and custody drain. The table lock brackets this latch
    /// with both reservation lookup and insertion-time predecessor validation.
    pub(super) fn fence_account_successor(
        &self,
        fence: &account_fence::DurableFence<'_>,
        busy: bool,
    ) -> bool {
        let relation = fence.relation();
        if self.predecessor != *relation.predecessor().prepare()
            || self.closed_phase() != relation.phase()
            || !self.noexec()
        {
            return false;
        }
        let request = relation.candidate();
        let mut succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        if succession.successor.as_ref() == Some(request) {
            return false;
        }
        match &succession.pending_account {
            Some(pending) if pending.request() == request => {
                succession.pending_account = Some(PendingAccount::Fenced(request.clone()));
            }
            None if busy && succession.successor.is_none() => {
                succession.pending_account = Some(PendingAccount::Fenced(request.clone()));
            }
            Some(_) if busy => return false,
            _ => {}
        }
        true
    }

    /// Called only after the exact candidate has no reservation, writer or
    /// undrained custody. A retry for C1 cannot release a pending C2.
    pub(super) fn release_fenced_account(&self, fence: &account_fence::DurableFence<'_>) {
        let mut succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(&succession.pending_account,
            Some(PendingAccount::Fenced(request)) if request == fence.relation().candidate())
        {
            succession.pending_account = None;
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedRestartEvidence",
            action = "Retire",
            project = "native_act::tests::closed_restart_model::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimPublication",
            action = "Retire",
            project = "native_act::tests::account_continuation_tests::claim_model::transition"
        )
    )]
    pub(super) fn retire(&self) {
        let mut succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
        succession.retired = true;
    }

    pub(super) fn cancelled_without_successor(
        &self,
        prepare: &Prepare,
        kernel: &dyn Kernel,
    ) -> bool {
        let cancelled = {
            let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
            succession.retired
                && succession.pending_account.is_none()
                && succession.successor.is_none()
        };
        self.predecessor == *prepare
            && cancelled
            && self.children.is_empty()
            && self.quiescent(kernel)
    }

    /// The last budget of a root that ended because its successor never attached: retired, with
    /// no continuation or account successor, as [`Self::cancelled_without_successor`] requires,
    /// except that a launcher it granted Execute may have run, so long as it exited with its group
    /// ([`Self::quiescent`]). Such a launcher never registered, or its root would have completed.
    pub(super) fn ended_without_successor(&self, prepare: &Prepare, kernel: &dyn Kernel) -> bool {
        let ended = {
            let succession = self.succession.lock().unwrap_or_else(|p| p.into_inner());
            succession.retired
                && succession.pending_account.is_none()
                && succession.successor.is_none()
        };
        self.predecessor == *prepare && ended && self.quiescent(kernel)
    }

    /// Recheck retained kernel objects and the exact drained input boundary.
    /// Source/child absence without those objects never passes this predicate.
    pub(super) fn quiescent(&self, kernel: &dyn Kernel) -> bool {
        let NativePlan::Restart { process, .. } = &self.predecessor.plan else {
            return false;
        };
        self.source.exited()
            && kernel.group_gone(process.group) == Some(true)
            && self.drain.as_ref().is_none_or(NativeDrain::is_drained)
            && self.children.iter().all(|(process, watch)| {
                watch.exited() && kernel.group_gone(process.group) == Some(true)
            })
    }
}
