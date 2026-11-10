// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Immutable accepted completion metadata; this cannot construct work custody.

use super::*;

/// Owned report data, never construction or source authority.
pub struct TargetReadyReport {
    pub preload_sequence: PreloadSequence,
    pub assignment_generation: u64,
    pub member: String,
    pub operation: String,
    pub registration: String,
    pub ready: Ready,
}

pub enum TargetReadyPreparation {
    Pending(Box<TargetReadyCommand>),
    Settled,
}

/// Existing counted PRELOAD capture retained before the transport may wait.
pub struct TargetReadyCommand {
    command: TargetConstruction,
    source: Arc<slot::SourceHandle>,
    body: Body,
    caller: slot::Caller,
}

impl std::fmt::Debug for TargetReadyCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetReadyCommand(redacted)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Body {
    preload_sequence: PreloadSequence,
    assignment_generation: u64,
    member: String,
    operation: String,
    registration: String,
    credential_generation: u64,
    digest: String,
    account: String,
    user: String,
    role: ReadyRole,
}

impl Body {
    pub(super) fn from_report(
        report: &TargetReady<'_>,
        role: ReadyRole,
        source: &slot::SourceHandle,
    ) -> Self {
        Self {
            preload_sequence: report.preload_sequence,
            assignment_generation: report.assignment_generation,
            member: report.member.into(),
            operation: report.operation.into(),
            registration: report.registration.into(),
            credential_generation: source.credential_generation(),
            digest: source.digest().into(),
            account: source.account().into(),
            user: source.user().into(),
            role,
        }
    }

    fn report(&self) -> TargetReady<'_> {
        let ready = match self.role {
            ReadyRole::Manager(auth) => Ready::Loaded {
                credential_generation: self.credential_generation,
                digest: self.digest.clone(),
                account: self.account.clone(),
                user: self.user.clone(),
                auth,
            },
            ReadyRole::ReaderLoaded => Ready::ReaderLoaded {
                credential_generation: self.credential_generation,
                digest: self.digest.clone(),
                account: self.account.clone(),
                user: self.user.clone(),
            },
            ReadyRole::PolicyUnavailable => Ready::PolicyUnavailable,
        };
        TargetReady {
            preload_sequence: self.preload_sequence,
            assignment_generation: self.assignment_generation,
            member: &self.member,
            operation: &self.operation,
            registration: &self.registration,
            ready,
        }
    }
}

impl Registry {
    pub fn prepare_target_ready(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        assignment: &str,
        context: &str,
        report: TargetReadyReport,
    ) -> Result<TargetReadyPreparation, Error> {
        let view = TargetReady {
            preload_sequence: report.preload_sequence,
            assignment_generation: report.assignment_generation,
            member: &report.member,
            operation: &report.operation,
            registration: &report.registration,
            ready: report.ready,
        };
        self.prepare_ready_report(caller, connection, assignment, context, view)
    }

    pub(super) fn prepare_ready_report(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        assignment: &str,
        context: &str,
        report: TargetReady<'_>,
    ) -> Result<TargetReadyPreparation, Error> {
        if !hex(assignment, 32)
            || !hex(context, 64)
            || !hex(report.member, 32)
            || !hex(report.operation, 32)
            || !hex(report.registration, 32)
            || report.assignment_generation == 0
            || !connection.is_live()
        {
            return Err(Error::Denied);
        }
        // Capture actual custody before observing the kernel outside State.
        let (cell, installed, source, body, process, command) = {
            let mut state = lock(&self.state)?;
            let cell = state
                .reservations
                .get(&actor_key(caller))
                .ok_or(Error::Stale)?
                .clone();
            let mut held = lock(&cell)?;
            let installed = held.installed.as_ref().ok_or(Error::Stale)?.clone();
            let attempt = preload_sequence::Attempt {
                sequence: report.preload_sequence,
                operation: report.operation.into(),
                origin: preload_sequence::Origin::Target {
                    assignment: assignment.into(),
                    context: context.into(),
                },
            };
            preload_sequence::check_current(
                &state,
                &member_key(caller, report.member)?,
                &held.process,
                &attempt,
            )?;
            if held.target.caller() != caller
                || held.target.operation() != assignment
                || installed.context != context
                || installed.assignment.generation() != report.assignment_generation
                || !allows_member(&state, &held, caller, report.member)
            {
                return Err(Error::Stale);
            }
            let member = state
                .members
                .get(&member_key(caller, report.member)?)
                .ok_or(Error::Stale)?;
            if member.process != held.process
                || member.source.assignment_generation() != report.assignment_generation
                || member.operation != report.operation
                || member.registration != report.registration
                || !member.connection.same(connection)
                || member.unknown
            {
                return Err(Error::Stale);
            }
            let desired = reported_role(&report.ready, &member.source)?;
            let source = member.source.clone();
            let body = Body::from_report(&report, desired, &source);
            let process = held.process.clone();
            let attempt_for_report = attempt.clone();
            let command = if member.readiness.role == Some(desired)
                && state
                    .preload_sequences
                    .get(&member_key(caller, report.member)?)
                    .is_some_and(|ledger| ledger.completed(report.registration))
            {
                None
            } else {
                let entry = held
                    .target_constructors
                    .values_mut()
                    .find(|entry| {
                        entry.work.member == report.member
                            && entry.preload.as_deref() == Some(report.operation)
                    })
                    .ok_or(Error::Stale)?;
                if !entry.connection.same(connection) || entry.work.worker.load(Ordering::Acquire) {
                    return Err(Error::Busy);
                }
                if entry.work.scope.cancelled()
                    || !state
                        .app_lifetime
                        .as_ref()
                        .is_some_and(|app| entry.work.scope.cancellation().owned_by(app))
                {
                    return Err(Error::Stale);
                }
                match &entry.ready {
                    Some(accepted) if accepted != &body => return Err(Error::Stale),
                    Some(_) => {}
                    None => entry.ready = Some(body.clone()),
                }
                let work = entry.work.clone();
                let kernel = match &held.phase {
                    seal::Phase::Sealed { kernel, .. } => kernel.clone(),
                    _ => return Err(Error::Stale),
                };
                Some(TargetConstruction {
                    state: Arc::downgrade(&self.state),
                    cell: Arc::downgrade(&cell),
                    installed: Arc::downgrade(&installed),
                    kernel,
                    work,
                    connection: connection.clone(),
                    process: process.clone(),
                    preload: Some(report.operation.into()),
                    preload_attempt: Some(attempt),
                })
            };
            if command.is_some() {
                state
                    .preload_sequences
                    .get_mut(&member_key(caller, report.member)?)
                    .ok_or(Error::Stale)?
                    .accept_ready(&attempt_for_report, report.registration, desired)?;
            }
            drop(held);
            (cell, installed, source, body, process, command)
        };
        if Process::observe(caller.pid).as_ref() != Some(&process) {
            return Err(Error::Stale);
        }
        let settled = {
            let state = lock(&self.state)?;
            same_cell(&state, &cell, caller)?;
            let held = lock(&cell)?;
            let member = state
                .members
                .get(&member_key(caller, report.member)?)
                .ok_or(Error::Stale)?;
            if held.process != process
                || !held
                    .installed
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &installed))
                || !Arc::ptr_eq(&member.source, &source)
                || member.operation != report.operation
                || member.registration != report.registration
                || !member.connection.same(connection)
                || !connection.is_live()
                || member.unknown
            {
                return Err(Error::Stale);
            }
            if member.readiness.role == Some(body.role) {
                true
            } else {
                let command = command.as_ref().ok_or(Error::Stale)?;
                if !held
                    .target_constructors
                    .get(&command.work.operation)
                    .is_some_and(|entry| {
                        Arc::ptr_eq(&entry.work, &command.work)
                            && entry.ready.as_ref() == Some(&body)
                            && entry.connection.same(connection)
                            && !entry.work.worker.load(Ordering::Acquire)
                    })
                {
                    return Err(Error::Stale);
                }
                false
            }
        };
        if settled {
            return Ok(TargetReadyPreparation::Settled);
        }
        Ok(TargetReadyPreparation::Pending(Box::new(
            TargetReadyCommand {
                command: command.ok_or(Error::Stale)?,
                source,
                body,
                caller: caller.clone(),
            },
        )))
    }

    pub fn run_target_ready(
        &self,
        env: &Env,
        completion: Box<TargetReadyCommand>,
    ) -> Result<(), Error> {
        let TargetReadyCommand {
            command,
            source,
            body,
            caller,
        } = *completion;
        let state = command.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state)
            || Process::observe(caller.pid).as_ref() != Some(&command.process)
        {
            return Err(Error::Stale);
        }
        command
            .work
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        let _worker = ConstructorWorker(command.work.clone());
        {
            let mut state = lock(&self.state)?;
            let attempt = command.preload_attempt.as_ref().ok_or(Error::Stale)?;
            state
                .preload_sequences
                .get_mut(&member_key(&caller, &body.member)?)
                .ok_or(Error::Stale)?
                .accept_ready(attempt, &body.registration, body.role)?;
        }
        let cell = command.cell.upgrade().ok_or(Error::Stale)?;
        let installed = command.installed.upgrade().ok_or(Error::Stale)?;
        let permission = InstalledRead {
            command: &command,
            installed: &installed,
            expected: &source,
        };
        permission.current().map_err(|_| Error::Stale)?;
        slot::validate_installed(env, &permission).map_err(|_| Error::Unavailable)?;
        let retired = {
            let mut state = lock(&self.state)?;
            let attempt = command.preload_attempt.as_ref().ok_or(Error::Stale)?;
            let key = member_key(&caller, &body.member)?;
            preload_sequence::check_current(&state, &key, &command.process, attempt)?;
            same_cell(&state, &cell, &caller)?;
            let mut held = lock(&cell)?;
            if !held
                .installed
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &installed))
                || !held
                    .target_constructors
                    .get(&command.work.operation)
                    .is_some_and(|entry| {
                        Arc::ptr_eq(&entry.work, &command.work)
                            && entry.connection.same(&command.connection)
                            && !entry.work.worker.load(Ordering::Acquire)
                            && entry.ready.as_ref() == Some(&body)
                    })
                || command.work.scope.cancelled()
            {
                return Err(Error::Stale);
            }
            resources::check_member_update(&state, &caller, &body.member)?;
            held.target_retiring.retain(|witness| !witness.retired());
            if held.target_retiring.len() >= MAX_MEMBERS {
                return Err(Error::Busy);
            }
            let member = state
                .members
                .get_mut(&member_key(&caller, &body.member)?)
                .ok_or(Error::Stale)?;
            if !Arc::ptr_eq(&member.source, &source)
                || member.operation != body.operation
                || member.registration != body.registration
                || !member.connection.same(&command.connection)
                || !command.connection.is_live()
                || member.unknown
            {
                return Err(Error::Stale);
            }
            let mut ledger = state
                .preload_sequences
                .get(&key)
                .ok_or(Error::Stale)?
                .clone();
            ledger.complete(attempt, &body.registration)?;
            state
                .members
                .get_mut(&key)
                .ok_or(Error::Stale)?
                .readiness
                .complete_recorded(reported_role(&body.report().ready, &source)?)?;
            state.preload_sequences.insert(key.clone(), ledger);
            held.target_retiring.push(command.work.scope.cancellation());
            held.target_constructors.remove(&command.work.operation)
        };
        drop(retired);
        Ok(())
    }
}
