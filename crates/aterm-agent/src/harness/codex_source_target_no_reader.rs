// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A destination constructor whose actual selected provider has no native
//! OAuth reader. This completion has no PRELOAD, source, generation or stamps.
use super::*;

pub struct TargetNoReaderRequest<'a> {
    pub assignment: &'a str,
    pub context: &'a str,
    pub member: &'a str,
    pub operation: &'a str,
    pub kind: NoNativeReaderKind,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Body {
    pub(super) kind: NoNativeReaderKind,
    pub(super) registration: String,
}
pub(super) struct Completion {
    pub(super) body: Body,
    pub(super) retirement: crate::harness::account_work::Cancellation,
}
pub enum TargetNoReaderPreparation {
    Pending(Box<TargetNoReaderCommand>),
    Settled { registration: String },
}
pub struct TargetNoReaderCommand {
    // B resources precede the constructor's original counted Scope in Drop.
    installed: Arc<Installed>,
    command: TargetConstruction,
    body: Body,
}

pub(super) fn observed<'a>(
    state: &'a State,
    caller: &slot::Caller,
    process: &Process,
    member: &str,
    installed: &Arc<Installed>,
) -> Option<&'a UnrelatedMember> {
    let key = member_key(caller, member).ok()?;
    if state.members.contains_key(&key) {
        return None;
    }
    let issued = state.actors.get(&actor_key(caller))?.issued.get(member)?;
    if issued
        .target
        .as_ref()
        .is_none_or(|target| !target.ptr_eq(&Arc::downgrade(installed)))
    {
        return None;
    }
    let record = state
        .target_opens
        .get(&open_key(caller, &issued.operation))?;
    let completion = record.no_reader.as_ref()?;
    let value = state.unrelated.get(&key)?;
    (record.caller == *caller
        && record.process == *process
        && record.member == member
        && record.context == installed.context
        && record.assignment == installed.assignment.operation()
        && record.validated
        && record.preloads.is_empty()
        && record.cleanup.is_none()
        && record.closed.is_none()
        && record.member_retirement.is_none()
        && completion.retirement.retired()
        && value.process == *process
        && value.operation == issued.operation
        && value.registration == completion.body.registration
        && value.kind == completion.body.kind
        && issued.connection.same(&value.connection)
        && value.connection.is_live())
    .then_some(value)
}

impl Registry {
    /// Retain only the actual original OPEN constructor; context bytes never
    /// create another constructor or a managed source permission.
    pub fn prepare_target_no_reader(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: TargetNoReaderRequest<'_>,
    ) -> Result<TargetNoReaderPreparation, Error> {
        if !hex(request.assignment, 32)
            || !hex(request.context, 64)
            || !hex(request.operation, 32)
            || !connection.is_live()
        {
            return Err(Error::Denied);
        }
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let key = member_key(caller, request.member)?;
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let state = lock(&self.state)?;
        let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
        let issued = actor.issued.get(request.member).ok_or(Error::Stale)?;
        let record = state
            .target_opens
            .get(&open_key(caller, request.operation))
            .ok_or(Error::Stale)?;
        if actor.process != process
            || issued.operation != request.operation
            || issued.target.is_none()
            || !issued.connection.same(connection)
            || record.process != process
            || record.member != request.member
            || record.assignment != request.assignment
            || record.context != request.context
            || !record.validated
            || !record.preloads.is_empty()
            || record.cleanup.is_some()
            || record.closed.is_some()
            || record.member_retirement.is_some()
            || state.members.contains_key(&key)
            || state.preload_sequences.contains_key(&key)
        {
            return Err(Error::Stale);
        }
        if let Some(completion) = &record.no_reader {
            let actual = state.unrelated.get(&key).ok_or(Error::Stale)?;
            if completion.body.kind != request.kind
                || actual.kind != request.kind
                || actual.operation != request.operation
                || actual.registration != completion.body.registration
            {
                return Err(Error::Stale);
            }
            // Exact recorded completion only. A replacement connection is not
            // evidence of a newly selected provider/current NoReader role.
            if !actual.connection.same(connection) {
                return Err(Error::Stale);
            }
            return Ok(TargetNoReaderPreparation::Settled {
                registration: actual.registration.clone(),
            });
        }
        let cell = state
            .reservations
            .get(&actor_key(caller))
            .ok_or(Error::Stale)?
            .clone();
        let mut held = lock(&cell)?;
        reservation_cell_scope(&state, &held)?;
        let installed = held.installed.as_ref().ok_or(Error::Stale)?.clone();
        let kernel = match &held.phase {
            seal::Phase::Sealed { kernel, .. } => kernel.clone(),
            _ => return Err(Error::Stale),
        };
        if held.process != process
            || installed.context != request.context
            || installed.assignment.operation() != request.assignment
            || issued
                .target
                .as_ref()
                .is_none_or(|issued| !issued.ptr_eq(&Arc::downgrade(&installed)))
        {
            return Err(Error::Stale);
        }
        resources::check_member_update(&state, caller, request.member)?;
        let entry = held
            .target_constructors
            .get_mut(request.operation)
            .ok_or(Error::Stale)?;
        if entry.work.member != request.member
            || !entry.connection.same(connection)
            || entry.preload.is_some()
            || entry.ready.is_some()
            || entry.work.scope.cancelled()
            || state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !entry.work.scope.cancellation().owned_by(app))
        {
            return Err(Error::Stale);
        }
        if entry.work.worker.load(Ordering::Acquire) {
            return Err(Error::Busy);
        }
        let body = if let Some(body) = &entry.no_reader {
            if body.kind != request.kind {
                return Err(Error::Stale);
            }
            body.clone()
        } else {
            let body = Body {
                kind: request.kind,
                registration,
            };
            entry.no_reader = Some(body.clone());
            body
        };
        let command = TargetConstruction {
            state: Arc::downgrade(&self.state),
            cell: Arc::downgrade(&cell),
            installed: Arc::downgrade(&installed),
            kernel,
            work: entry.work.clone(),
            connection: connection.clone(),
            process,
            preload: None,
            preload_attempt: None,
        };
        Ok(TargetNoReaderPreparation::Pending(Box::new(
            TargetNoReaderCommand {
                installed,
                command,
                body,
            },
        )))
    }

    pub fn run_target_no_reader(
        &self,
        pending: Box<TargetNoReaderCommand>,
    ) -> Result<String, Error> {
        let installed = &pending.installed;
        let command = &pending.command;
        let body = &pending.body;
        if command
            .state
            .upgrade()
            .is_none_or(|state| !Arc::ptr_eq(&state, &self.state))
            || Process::observe(installed.assignment.caller().pid).as_ref()
                != Some(&command.process)
        {
            return Err(Error::Stale);
        }
        command
            .work
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        let _worker = ConstructorWorker(command.work.clone());
        command.kernel.current().map_err(|_| Error::Stale)?;
        installed.resource.current().map_err(|_| Error::Stale)?;
        let retired = {
            let mut state = lock(&self.state)?;
            let caller = installed.assignment.caller();
            let key = member_key(caller, &command.work.member)?;
            let cell = command.cell.upgrade().ok_or(Error::Stale)?;
            same_cell(&state, &cell, caller)?;
            let mut held = lock(&cell)?;
            reservation_cell_scope(&state, &held)?;
            if !matches!(&held.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, &command.kernel))
                || !held
                    .installed
                    .as_ref()
                    .is_some_and(|value| Arc::ptr_eq(value, installed))
                || command.work.scope.cancelled()
                || !command.connection.is_live()
                || state.members.contains_key(&key)
                || state.preload_sequences.contains_key(&key)
            {
                return Err(Error::Stale);
            }
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            let issued = actor.issued.get(&command.work.member).ok_or(Error::Stale)?;
            let entry = held
                .target_constructors
                .get(&command.work.operation)
                .ok_or(Error::Stale)?;
            let open = open_key(caller, &command.work.operation);
            let record = state.target_opens.get(&open).ok_or(Error::Stale)?;
            if actor.process != command.process
                || !issued.connection.same(&command.connection)
                || !Arc::ptr_eq(&entry.work, &command.work)
                || entry.no_reader.as_ref() != Some(body)
                || !entry.connection.same(&command.connection)
                || entry.preload.is_some()
                || !record.preloads.is_empty()
                || record.cleanup.is_some()
                || record.closed.is_some()
                || record.member_retirement.is_some()
            {
                return Err(Error::Stale);
            }
            resources::check_member_update(&state, caller, &command.work.member)?;
            held.target_retiring.retain(|witness| !witness.retired());
            if held.target_retiring.len() >= MAX_MEMBERS {
                return Err(Error::Busy);
            }
            version(&mut state)?;
            state.unrelated.insert(
                key,
                UnrelatedMember {
                    process: command.process.clone(),
                    operation: command.work.operation.clone(),
                    registration: body.registration.clone(),
                    connection: command.connection.clone(),
                    kind: body.kind,
                },
            );
            state
                .target_opens
                .get_mut(&open)
                .ok_or(Error::Stale)?
                .no_reader = Some(Completion {
                body: body.clone(),
                retirement: command.work.scope.cancellation(),
            });
            held.target_retiring.push(command.work.scope.cancellation());
            held.target_constructors.remove(&command.work.operation)
        };
        drop(retired);
        Ok(body.registration.clone())
    }
}

/// Resources must drop before releasing the App's counted cleanup turn.
pub(super) struct Retirement {
    _resources: Option<resources::ResourceRetirement>,
    _turn: Option<Arc<crate::harness::account_work::ReleaseTurn>>,
}
pub(super) fn retire_completed(
    registry: &Registry,
    cleanup: &TargetOpenCleanup,
) -> Result<Option<Retirement>, Error> {
    let mut state = lock(&registry.state)?;
    let open = open_key(&cleanup.caller, &cleanup.open);
    let record = state.target_opens.get(&open).ok_or(Error::Stale)?;
    let Some(completion) = &record.no_reader else {
        return Ok(None);
    };
    if record.caller != cleanup.caller
        || record.process != cleanup.process
        || record.cleanup.as_ref() != Some(&cleanup.request)
        || cleanup
            .request
            .context
            .as_ref()
            .is_some_and(|(assignment, context)| {
                record.assignment != *assignment || record.context != *context
            })
        || cleanup
            .request
            .member
            .as_ref()
            .is_some_and(|member| member != &record.member)
    {
        return Err(Error::Stale);
    }
    if let Some(closed) = &record.closed {
        return if closed == &cleanup.request {
            Ok(Some(Retirement {
                _resources: None,
                _turn: None,
            }))
        } else {
            Err(Error::Stale)
        };
    }
    if !completion.retirement.retired()
        || cleanup._turn.is_none()
        || record.cleanup_turn.is_none()
        || !record.preloads.is_empty()
    {
        return Err(Error::Busy);
    }
    let member = record.member.clone();
    let key = member_key(&cleanup.caller, &member)?;
    let actual = state.unrelated.get(&key).ok_or(Error::Stale)?;
    if actual.registration != completion.body.registration
        || actual.kind != completion.body.kind
        || actual.operation != cleanup.open
        || state.members.contains_key(&key)
    {
        return Err(Error::Stale);
    }
    resources::check_retirement(&state, &cleanup.caller, &member)?;
    resources::check_retired_member_captures(&state, &cleanup.caller, &member)?;
    let retired = resources::revoke_member(&mut state, &cleanup.caller, &member);
    let record = state.target_opens.get_mut(&open).ok_or(Error::Stale)?;
    record.closed = Some(cleanup.request.clone());
    let turn = record.cleanup_turn.take();
    state.unrelated.remove(&key);
    if let Some(actor) = state.actors.get_mut(&actor_key(&cleanup.caller)) {
        actor.issued.remove(&member);
    }
    // The actual closed fence is retained in this native incarnation;
    // external handles retain metadata only. No absence/HWM was used as proof.
    drop(state);
    Ok(Some(Retirement {
        _resources: Some(retired),
        _turn: turn,
    }))
}

#[cfg(all(test, unix))]
#[path = "codex_source_target_no_reader_tests.rs"]
mod tests;

/// The caller already proved exact actor departure outside State. Return any
/// retained cleanup turns with their records; never drop them under State.
pub(in crate::harness::codex_source_registry) fn take_departed_open_custody(
    state: &mut State,
    actors: &[(String, Process)],
) -> Vec<OpenRecord> {
    let keys = state
        .target_opens
        .iter()
        .filter(|(_, record)| {
            actors.iter().any(|(actor, process)| {
                actor_key(&record.caller) == *actor && record.process == *process
            })
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| state.target_opens.remove(&key))
        .collect()
}
