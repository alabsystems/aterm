// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication and admission of an immutable native preparation are separate.

use super::*;

#[derive(Debug)]
pub(super) struct Preparation {
    predecessor: Option<Box<(Prepare, Binding)>>,
    pub(super) recorded: bool,
    account_state: Option<aterm_agent::harness::native_account_wire::ChildState>,
    serial: u64,
    active: Option<u64>,
}

impl Preparation {
    pub(super) fn idle(&self) -> bool {
        self.active.is_none()
    }

    pub(super) fn terminal_already_recorded(&self) -> bool {
        use aterm_agent::harness::native_account_wire::ChildState;
        matches!(
            self.account_state,
            Some(ChildState::MayExecute | ChildState::Sealed { .. })
        )
    }

    pub(super) fn transfers(&self, request: &Prepare, binding: &Binding) -> bool {
        self.recorded
            && self
                .predecessor
                .as_deref()
                .is_some_and(|(parent, capability)| parent == request && capability == binding)
    }

    /// A typed claim cannot acknowledge a writer that still owns admission.
    pub(super) fn settled_account(&self, request: &Prepare, binding: &Binding) -> bool {
        self.active.is_none() && self.transfers(request, binding)
    }

    #[cfg(test)]
    pub(super) fn recorded_for_test() -> Self {
        let mut preparation = Self::new(false, None);
        preparation.release(1);
        preparation
    }

    pub(super) fn new(required: bool, predecessor: Option<(Prepare, Binding)>) -> Self {
        Self {
            predecessor: predecessor.map(Box::new),
            recorded: !required,
            account_state: None,
            serial: 1,
            active: Some(1),
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "Begin",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "Collide",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    fn begin(&mut self) -> Option<u64> {
        if self.active.is_some() {
            return None;
        }
        let serial = self.serial.checked_add(1)?;
        self.serial = serial;
        self.active = Some(serial);
        Some(serial)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "Ack",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "StaleAck",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PreparationPublication",
            action = "LostAck",
            reason = "A committed append can lose its acknowledgement without informing the writer"
        )
    )]
    fn acknowledge(&mut self, serial: u64) -> bool {
        if self.active != Some(serial) {
            return false;
        }
        self.recorded = true;
        true
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "Release",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    fn release(&mut self, serial: u64) {
        if self.active == Some(serial) {
            self.active = None;
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "Admit",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PreparationPublication",
            action = "Invalidate",
            reason = "Current authority, conversation or deadline is an independently checked premise"
        )
    )]
    fn can_admit(&self, current: bool) -> bool {
        self.recorded
            && self.active.is_some()
            && current
            && self.account_state.is_none_or(|state| {
                state == aterm_agent::harness::native_account_wire::ChildState::Prepared
            })
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PreparationPublication",
            action = "Publish",
            project = "aterm_gui::native_act::preparation::tests::transition"
        )
    )]
    pub(super) fn can_publish(&self) -> bool {
        self.recorded
    }
}

/// The writer owns admission too. A concurrent exact replay observes Unknown
/// before acknowledgement, then Pending until this attempt releases its token.
pub(super) struct Preparing {
    pub(super) operation: Arc<Mutex<Operation>>,
    pub(super) serial: u64,
    pub(super) initial_custody: Option<Arc<NativeCustody>>,
}

impl Drop for Preparing {
    fn drop(&mut self) {
        let custody = {
            let mut op = self.operation.lock().unwrap_or_else(|p| p.into_inner());
            if op.preparation.active != Some(self.serial) {
                return;
            }
            if op.phase == Phase::Queued {
                if let Some(custody) = &op.custody {
                    op.completion_drain = Some(custody.drain());
                }
                op.channel = None;
                op.channel_closed = false;
                op.custody.take()
            } else {
                None
            }
        };
        // Never keep input or app-handoff custody across an uncertain append.
        // Release the marker before another attempt can reserve a new one.
        drop(custody);
        drop(self.initial_custody.take());
        self.operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .preparation
            .release(self.serial);
    }
}

pub(super) fn preparation_pending_reply(op: &Operation) -> String {
    if !op.preparation.can_publish() {
        return UNKNOWN.into();
    }
    format!(
        "OK native-act state=pending id={} cap={}\n",
        op.binding.id, op.binding.capability
    )
}

pub(super) fn retry_preparation(
    ctx: &crate::SessionCtx,
    master: i32,
    operation: Arc<Mutex<Operation>>,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    admit: bool,
) -> String {
    let connection = if admit {
        let Some(connection) = caller.connection else {
            return DENIED.into();
        };
        Some(connection)
    } else {
        None
    };
    let serial = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        // A stale Arc captured before fence cleanup is not a new reservation.
        // It cannot restart the old writer once its pending proof was fenced
        // or released for a different candidate.
        if !op.preparation.recorded
            && op
                .authority
                .account
                .as_ref()
                .is_some_and(|account| !account.proof().recoverable_account(&op.request))
        {
            return UNKNOWN.into();
        }
        let Some(serial) = op.preparation.begin() else {
            return if admit {
                preparation_pending_reply(&op)
            } else {
                UNKNOWN.into()
            };
        };
        if let Some(connection) = connection {
            op.channel = Some(connection);
            op.channel_closed = false;
            table.coordinated_connections.insert(connection);
        }
        serial
    };
    finish_preparation(
        ctx,
        master,
        Preparing {
            operation,
            serial,
            initial_custody: None,
        },
        caller,
        kernel,
        admit,
    )
}

fn preparation_current(
    ctx: &crate::SessionCtx,
    master: i32,
    op: &Operation,
    meta: &crate::session_timeline::SessionMeta,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    preflight: bool,
) -> bool {
    op.preparation.can_admit(authority_current(
        ctx, master, op, meta, caller, kernel, preflight,
    ))
}

pub(super) fn authority_current(
    ctx: &crate::SessionCtx,
    master: i32,
    op: &Operation,
    meta: &crate::session_timeline::SessionMeta,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    preflight: bool,
) -> bool {
    let pending = !op.preparation.recorded;
    let crash_proven = op.observation.as_ref().is_some_and(|proof| {
        observe_control::context(ctx, meta, caller, kernel).is_some_and(|context| {
            proof.reprove_with(
                &context,
                master,
                op.clock.now_us().unwrap_or(u64::MAX),
                &ObservationKernel(kernel),
            )
        })
    });
    preflight
        && same_actor(op, caller, kernel)
        && op.authority.asked_matches(&op.request)
        && kernel.constructor_enabled(op.request.constructor())
        && meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot
        && op.authority.renewal.as_ref().is_none_or(|seal| {
            seal.admits(&op.authority.snapshot, &op.request)
                && seal
                    .continuation()
                    .is_none_or(|proof| proof.claimed_by(&op.request))
        })
        && op.authority.account.as_ref().is_none_or(|account| {
            account.current(ctx, &op.request, caller, kernel)
                && if pending {
                    account.proof().pending_account_by(&op.request)
                } else {
                    account.proof().claimed_by(&op.request)
                }
        })
        && (!matches!(op.request.plan, NativePlan::Restart { .. })
            || ctx.human_input.seq() == op.human_seq)
        && validate_plan(&op.request, &op.authority, master, kernel, crash_proven)
        && op
            .authority
            .conversation
            .as_ref()
            .is_none_or(|guard| guard.current(ctx))
        && op.clock.remaining(op.request.deadline_us).is_some()
}

pub(super) fn finish_preparation(
    ctx: &crate::SessionCtx,
    master: i32,
    mut attempt: Preparing,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
    admit: bool,
) -> String {
    let operation = &attempt.operation;
    let (prepare, binding, predecessor, recorded, handoff, renewal, account, mut account_state) = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        (
            op.request.clone(),
            op.binding.clone(),
            op.preparation.predecessor.clone(),
            op.preparation.recorded,
            op.custody
                .as_ref()
                .or(attempt.initial_custody.as_ref())
                .map(|custody| custody.handoff_guard()),
            op.authority.renewal.clone(),
            op.authority.account.clone(),
            op.preparation.account_state,
        )
    };
    // A retry can have no input custody. Its app hold lasts only this worker,
    // never the wait for a caller, a future disk recovery or a fresh authority.
    let _handoff = match handoff.or_else(|| {
        aterm_types::update_boundary::Boundary::process()
            .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
            .map(Arc::new)
    }) {
        Some(handoff) => handoff,
        None => return UNKNOWN.into(),
    };
    let _predecessor_flight = match predecessor.as_deref() {
        Some((parent, _)) => match ReceiptFlight::begin(ctx, &Request::prepare(parent.clone())) {
            Ok(flight) => flight,
            Err(()) => return UNKNOWN.into(),
        },
        None => None,
    };
    // A published preparation can remain queued while custody or live facts
    // are unavailable. Before admitting that retry, recover the journal again:
    // a cached Prepared classification cannot override a later terminal frame.
    if recorded
        && let Some(account) = &account
        && account_state.is_none_or(|state| {
            state == aterm_agent::harness::native_account_wire::ChildState::Prepared
        })
    {
        let Some(recovered) = kernel.recover_account_claim(account.relation(), &binding) else {
            return UNKNOWN.into();
        };
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !op.preparation.acknowledge(attempt.serial) {
            return UNKNOWN.into();
        }
        op.preparation.account_state = Some(recovered);
        account_state = Some(recovered);
    }
    if !recorded {
        let confirmed = if let Some(account) = &account {
            account_state = kernel.recover_account_claim(account.relation(), &binding);
            if account_state.is_none() && admit {
                let guard = account.admit(kernel);
                let fresh = {
                    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
                    !op.channel_closed
                        && !aterm_control::hangup::caller_hung_up()
                        && authority_current(ctx, master, &op, &meta, caller, kernel, guard.is_ok())
                };
                drop(guard);
                if fresh {
                    account_state = kernel.record_account_claim(account.relation(), &binding);
                }
            }
            account_state.is_some()
        } else if let Some((parent, parent_binding)) = predecessor.as_deref() {
            kernel.record_successor(parent, parent_binding, &prepare, &binding)
        } else {
            kernel.record_prepared(&prepare, &binding)
        };
        if !confirmed {
            return UNKNOWN.into();
        }
        if account
            .as_ref()
            .is_some_and(|account| !account.proof().confirm_account_successor(&prepare))
        {
            return UNKNOWN.into();
        }
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !op.preparation.acknowledge(attempt.serial) {
            return UNKNOWN.into();
        }
        op.preparation.account_state = account_state;
    }
    if account_state.is_some_and(|state| {
        state != aterm_agent::harness::native_account_wire::ChildState::Prepared
    }) {
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        let completion = match account_state.expect("recorded account state") {
            aterm_agent::harness::native_account_wire::ChildState::NoExec(phase) => {
                if command_for(&op, phase).is_none()
                    || retirement(&op, kernel) != Some(Retirement::NotGranted)
                {
                    return UNKNOWN.into();
                }
                // Disk identifies the phase; retained kernel evidence alone
                // permits constructing the closed witness for that phase.
                op.resume_phase = Some(phase);
                op.seal_retirement(Retirement::NotGranted, kernel)
            }
            _ => op.seal_completion(Decision::Unknown),
        };
        drop(op);
        return finish(ctx, operation, completion);
    }
    // The original worker retains its marker across cancellation until earlier
    // admitted input drains. An uncertain write above instead releases it in
    // Preparing::drop; it never waits for unrelated input while storage fails.
    let initial_drained = if let Some(custody) = attempt.initial_custody.take() {
        let ready = custody.await_predecessors(prepare.deadline_us);
        drop(custody);
        ready
    } else {
        true
    };
    // Storage recovery is permitted after expiry; admission is not. Account
    // and renewal reads remain outside Meta and the operation predicate.
    let account_guard = account
        .as_ref()
        .map_or(Ok(None), |account| account.admit(kernel));
    let preflight = account_guard.is_ok() && renewal.as_ref().is_none_or(|seal| seal.preflight());
    let custody = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(op.phase, Phase::Completed(_)) {
            drop(op);
            drop(meta);
            drop(account_guard);
            return completed_reply(operation);
        }
        if op.phase != Phase::Queued {
            return UNKNOWN.into();
        }
        if !initial_drained
            || (admit && (op.channel_closed || aterm_control::hangup::caller_hung_up()))
            || !preparation_current(ctx, master, &op, &meta, caller, kernel, preflight)
        {
            let completion = abandon_preparation(&mut op, kernel);
            drop(op);
            drop(meta);
            drop(account_guard);
            kernel.completion_sealed();
            return finish(ctx, operation, completion);
        }
        if !admit {
            return UNKNOWN.into();
        }
        if op.custody.is_none() {
            let Some(boundary) = aterm_types::update_boundary::Boundary::process()
                .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
            else {
                return preparation_pending_reply(&op);
            };
            let Ok(custody) =
                ctx.fenced_writes
                    .reserve_native(op.request.revision, &op.request.id, boundary)
            else {
                return preparation_pending_reply(&op);
            };
            op.completion_drain = None;
            op.custody = Some(Arc::new(custody));
        }
        Arc::clone(op.custody.as_ref().expect("reserved native custody"))
    };
    drop(account_guard);
    let ready = custody.await_predecessors(prepare.deadline_us);
    drop(custody);
    let account_guard = account
        .as_ref()
        .map_or(Ok(None), |account| account.admit(kernel));
    let preflight = account_guard.is_ok() && renewal.as_ref().is_none_or(|seal| seal.preflight());
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if matches!(op.phase, Phase::Completed(_)) {
        drop(op);
        drop(table);
        drop(meta);
        drop(account_guard);
        return completed_reply(operation);
    }
    if !ready
        || op.channel_closed
        || aterm_control::hangup::caller_hung_up()
        || !preparation_current(ctx, master, &op, &meta, caller, kernel, preflight)
    {
        let completion = abandon_preparation(&mut op, kernel);
        drop(op);
        drop(table);
        drop(meta);
        drop(account_guard);
        kernel.completion_sealed();
        return finish(ctx, operation, completion);
    }
    if !relaunch_permit::finish_admission(&mut table, &op) {
        let completion = abandon_preparation(&mut op, kernel);
        drop(op);
        drop(table);
        drop(meta);
        drop(account_guard);
        kernel.completion_sealed();
        return finish(ctx, operation, completion);
    }
    op.phase = Phase::Started;
    if matches!(op.request.plan, NativePlan::OwnImage { .. }) {
        op.effect_started
            .store(true, std::sync::atomic::Ordering::Release);
    }
    admitted(&op.binding)
}

fn abandon_preparation(op: &mut Operation, kernel: &dyn Kernel) -> Option<Completion> {
    // A continuation owes the original conversation. Temporary uncertainty
    // about its drained cohort must not become an irreversible opaque seal.
    if op.resume_only && retirement(op, kernel) != Some(Retirement::NotGranted) {
        return None;
    }
    op.seal_without_effect(kernel)
}

/// The original issuer may finish its own terminal bookkeeping after a logical
/// service was cancelled. This path cannot admit, signal, resume or execute.
pub(super) fn settle_retired(
    ctx: &crate::SessionCtx,
    operation: Arc<Mutex<Operation>>,
    kernel: &dyn Kernel,
) -> bool {
    let (binding, caller, queued, handoff) = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        (
            op.binding.clone(),
            op.authority.caller.clone(),
            op.phase == Phase::Queued,
            op.custody.as_ref().map(|custody| custody.handoff_guard()),
        )
    };
    let Ok(_flight) = ReceiptFlight::begin(ctx, &Request::Query(binding)) else {
        return false;
    };
    let Some(_handoff) = handoff.or_else(|| {
        aterm_types::update_boundary::Boundary::process()
            .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
            .map(Arc::new)
    }) else {
        return false;
    };
    if queued {
        let _reply = retry_preparation(
            ctx,
            ctx.sink.master(),
            Arc::clone(&operation),
            &caller,
            kernel,
            false,
        );
    }
    let (prepare, binding, record) = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        let Some(record) = terminal_record(&op, UNKNOWN) else {
            return false;
        };
        (op.request.clone(), op.binding.clone(), record)
    };
    match record {
        TerminalRecord::NoExec(phase) => kernel.record_noexec(&prepare, &binding, phase),
        TerminalRecord::Sealed => kernel.record_sealed(&prepare, &binding),
    }
}

#[cfg(test)]
#[path = "native_preparation_tests.rs"]
mod tests;
