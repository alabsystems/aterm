// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, data-only succession after a positively settled native forward move.
//! Historical settlement is not current process, account or person authority.

use super::*;

#[derive(Debug, Default)]
pub(super) enum CurrentBinding {
    #[default]
    Unknown,
    Unsettled {
        binding: Binding,
        prior: Option<Box<SettledBinding>>,
    },
    Settled(Box<SettledBinding>),
}

#[derive(Clone, Debug)]
pub(super) struct SettledBinding {
    binding: Binding,
    host_epoch: String,
    vendor: Process,
    shell: Process,
    conversation: Conversation,
    authority: SupervisorAuthority,
}

/// Only this host derives a qualification, and only for this exact new preparation.
#[derive(Clone, Debug)]
pub(super) struct Qualification {
    request: Prepare,
}

impl Qualification {
    pub(super) fn admits(&self, request: &Prepare) -> bool {
        self.request == *request && shape(request)
    }
}

pub(super) fn shape(request: &Prepare) -> bool {
    request.duty == Scope::Upgrade
        && request
            .asked_upgrade
            .as_ref()
            .is_some_and(|asked| aterm_codec::hex::is_lower(&asked.predecessor, 32))
        && matches!(
            request.plan,
            NativePlan::Restart {
                observation: None,
                cold: None,
                ..
            }
        )
}

impl Operations {
    /// Called only at accepted publication, after actual authority/custody checks.
    pub(super) fn admitted_binding(&mut self, op: &Operation) {
        let request = &op.request;
        let previous = std::mem::take(&mut self.asked_binding);
        let prior = match previous {
            CurrentBinding::Settled(settled)
                if request.asked_upgrade.as_ref().is_some_and(|asked| {
                    shape(request)
                        && op.authority.asked_matches(request)
                        && asked.predecessor == settled.binding.id
                }) =>
            {
                Some(settled)
            }
            _ => None,
        };
        self.asked_binding = CurrentBinding::Unsettled {
            binding: op.binding.clone(),
            prior,
        };
    }

    /// Preserve positive settlement before ordinary bounded history reclamation.
    /// No kernel query, I/O, resource owner or mutation qualification is created here.
    pub(super) fn settle_asked_binding(&mut self) -> Result<(), &'static str> {
        let binding = match &self.asked_binding {
            CurrentBinding::Unsettled { binding, .. } => binding,
            CurrentBinding::Settled(_) => return Ok(()),
            CurrentBinding::Unknown => return Err("settlement-unknown"),
        };
        if self.receipt_flights.contains_key(&binding.id) || self.account_lineages.pins(&binding.id)
        {
            return Err("settlement-receipt-or-account-debt");
        }
        let Some(operation) = self.by_id.get(&binding.id) else {
            return Err("settlement-operation-absent");
        };
        let Ok(op) = operation.try_lock() else {
            return Err("settlement-operation-busy");
        };
        if op.binding != *binding {
            return Err("settlement-binding");
        }
        let Phase::Completed(decision) = op.phase else {
            return Err("settlement-not-completed");
        };
        if op.terminal_acknowledged != Some(decision) {
            return Err("settlement-terminal-ack");
        }
        if !op.preparation.recorded {
            return Err("settlement-preparation-record");
        }
        if !op.authority.builtin {
            return Err("settlement-not-builtin");
        }
        if op.continuation_pending() {
            return Err("settlement-continuation");
        }
        if op.cold_resolving {
            return Err("settlement-cold-resolving");
        }
        if op.custody.is_some() {
            return Err("settlement-custody");
        }
        if op
            .completion_drain
            .as_ref()
            .is_some_and(|drain| !drain.is_drained())
        {
            return Err("settlement-drain");
        }
        if !matches!(op.request.plan, NativePlan::Restart { cold: Some(_), .. })
            && op.request.asked_upgrade.is_none()
        {
            return Err("settlement-constructor");
        }
        if decision == Decision::ProvenNotCommittedRollbackSafe {
            // A rollback-safe result after any accepted effect is not a retry grant.
            if op.request.asked_upgrade.is_none()
                || !op.authority.asked_matches(&op.request)
                || op.signal_sent
                || op.resume_sent
                || op.resume_attempt.is_some()
                || op.grant_record != GrantRecord::None
                || op.children.iter().any(Option::is_some)
            {
                return Err("settlement-abort-effect");
            }
            drop(op);
            let previous = std::mem::take(&mut self.asked_binding);
            let CurrentBinding::Unsettled { binding, prior } = previous else {
                unreachable!("the table exclusively owns the current binding");
            };
            self.asked_binding = match prior {
                Some(settled) => CurrentBinding::Settled(settled),
                None => CurrentBinding::Unsettled {
                    binding,
                    prior: None,
                },
            };
            return if matches!(self.asked_binding, CurrentBinding::Settled(_)) {
                Ok(())
            } else {
                Err("settlement-abort-predecessor")
            };
        }
        if decision != Decision::Committed {
            return Err("settlement-not-committed");
        }
        if op.resume_phase != Some(ImagePhase::Forward) {
            return Err("settlement-not-forward");
        }
        let Some(outcome) = &op.committed_image else {
            return Err("settlement-kernel-outcome");
        };
        let Some(conversation) = &op.authority.conversation else {
            return Err("settlement-conversation");
        };
        let NativePlan::Restart { shell, .. } = &op.request.plan else {
            return Err("settlement-plan");
        };
        let Some(command) = command_for(&op, ImagePhase::Forward) else {
            return Err("settlement-resolved-command");
        };
        if outcome.process.image != *command.settled() {
            return Err("settlement-resulting-image");
        }
        let settled = SettledBinding {
            binding: binding.clone(),
            host_epoch: op.request.host_epoch.clone(),
            vendor: outcome.process.clone(),
            shell: shell.clone(),
            conversation: conversation.clone(),
            authority: op.authority.snapshot.clone(),
        };
        drop(op);
        self.asked_binding = CurrentBinding::Settled(Box::new(settled));
        Ok(())
    }

    pub(super) fn qualify_asked(
        &mut self,
        ctx: &crate::SessionCtx,
        request: &Prepare,
        authority: &Authority,
        master: i32,
        kernel: &dyn Kernel,
    ) -> Result<Qualification, &'static str> {
        if !shape(request)
            || !authority.builtin
            || authority.renewal.is_some()
            || authority.account.is_some()
        {
            return Err("qualification-caller-or-shape");
        }
        self.settle_asked_binding()?;
        let CurrentBinding::Settled(settled) = &self.asked_binding else {
            return Err("qualification-no-settled-binding");
        };
        let asked = request
            .asked_upgrade
            .as_ref()
            .ok_or("qualification-no-intent")?;
        let NativePlan::Restart { process, shell, .. } = &request.plan else {
            return Err("qualification-plan");
        };
        if asked.predecessor != settled.binding.id {
            return Err("qualification-predecessor");
        }
        if request.host_epoch != settled.host_epoch {
            return Err("qualification-request-epoch");
        }
        if host_epoch() != Some(settled.host_epoch.as_str()) {
            return Err("qualification-current-epoch");
        }
        if process != &settled.vendor {
            return Err("qualification-resulting-vendor");
        }
        if shell != &settled.shell {
            return Err("qualification-resulting-shell");
        }
        if !settled.conversation.current(ctx) {
            return Err("qualification-conversation");
        }
        if !settled
            .authority
            .same_builtin_relaunch_continuity(&authority.snapshot)
        {
            return Err("qualification-builtin-continuity");
        }
        if kernel.process(process.pid).as_ref() != Some(process) {
            return Err("qualification-live-vendor");
        }
        if kernel.process(shell.pid).as_ref() != Some(shell) {
            return Err("qualification-live-shell");
        }
        if kernel.foreground(master) != Some(process.group) {
            return Err("qualification-foreground");
        }
        if request.expected_human_seq != Some(ctx.human_input.seq()) {
            return Err("qualification-human-sequence");
        }
        if self.receipt_flights.contains_key(&settled.binding.id) {
            return Err("qualification-receipt-flight");
        }
        if self.account_lineages.pins(&settled.binding.id) {
            return Err("qualification-account-lineage");
        }
        Ok(Qualification {
            request: request.clone(),
        })
    }
}

/// A positive terminal append belongs to the exact captured operation, never a reused id.
pub(super) fn acknowledge_terminal(
    ctx: &crate::SessionCtx,
    operation: &Arc<Mutex<Operation>>,
    request: &Prepare,
    binding: &Binding,
    decision: Decision,
) {
    let table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if !table
        .by_id
        .get(&binding.id)
        .is_some_and(|current| Arc::ptr_eq(current, operation))
    {
        return;
    }
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if op.request == *request
        && op.binding == *binding
        && op.phase == Phase::Completed(decision)
        && matches!(
            decision,
            Decision::Committed | Decision::ProvenNotCommittedRollbackSafe
        )
    {
        op.terminal_acknowledged = Some(decision);
    }
}
