// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A target resample retains the original installed B constructor, its actual
//! Scope and kernel borrow. It never reconstructs that role from context bytes.
use super::*;

pub struct TargetResampleRequest<'a> {
    pub assignment: &'a str,
    pub context: &'a str,
    pub member: &'a str,
    pub operation: &'a str,
    pub sequence: PreloadSequence,
    pub previous: &'a PreloadPredecessor,
}
pub enum TargetResamplePreparation {
    Pending(Box<TargetResampleCommand>),
    Settled(PreloadResample),
}
pub struct TargetResampleCommand {
    command: TargetConstruction,
    previous: PreloadPredecessor,
}

fn outcome(
    status: preload_sequence::ResampleAdmission,
    previous: &PreloadPredecessor,
) -> Option<PreloadResample> {
    match status {
        preload_sequence::ResampleAdmission::Replay(result) => {
            Some(PreloadResample::Preload(result))
        }
        preload_sequence::ResampleAdmission::ReadyCommitted => {
            Some(PreloadResample::ReadyCommitted {
                previous: previous.clone(),
            })
        }
        preload_sequence::ResampleAdmission::Read => None,
    }
}
impl Registry {
    pub fn prepare_target_resample(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: TargetResampleRequest<'_>,
    ) -> Result<TargetResamplePreparation, Error> {
        if !hex(request.assignment, 32) || !hex(request.context, 64) || !hex(request.operation, 32)
        {
            return Err(Error::Denied);
        }
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let key = member_key(caller, request.member)?;
        let attempt = preload_sequence::Attempt {
            sequence: request.sequence,
            operation: request.operation.into(),
            origin: preload_sequence::Origin::Target {
                assignment: request.assignment.into(),
                context: request.context.into(),
            },
        };
        let mut state = lock(&self.state)?;
        let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
        if actor.process != process {
            return Err(Error::Stale);
        }
        if let Some(result) = outcome(
            state
                .preload_sequences
                .get(&key)
                .ok_or(Error::Stale)?
                .resample_status(&attempt, request.previous)?,
            request.previous,
        ) {
            return Ok(TargetResamplePreparation::Settled(result));
        }
        let issued = actor.issued.get(request.member).ok_or(Error::Stale)?;
        if !connection.is_live() || !issued.connection.same(connection) {
            return Err(Error::Stale);
        }
        let open = issued.operation.clone();
        let record = state
            .target_opens
            .get(&open_key(caller, &open))
            .ok_or(Error::Stale)?;
        if record.process != process
            || record.assignment != request.assignment
            || record.context != request.context
        {
            return Err(Error::Stale);
        }
        record.admits_preload(request.member, request.operation)?;
        let cell = state
            .reservations
            .get(&actor_key(caller))
            .ok_or(Error::Stale)?
            .clone();
        let held = lock(&cell)?;
        reservation_cell_scope(&state, &held)?;
        let installed = held.installed.as_ref().ok_or(Error::Stale)?;
        let kernel = match &held.phase {
            seal::Phase::Sealed { kernel, .. } => kernel.clone(),
            _ => return Err(Error::Stale),
        };
        if held.process != process
            || held.target.operation() != request.assignment
            || installed.context != request.context
            || issued
                .target
                .as_ref()
                .is_none_or(|issued| !issued.ptr_eq(&Arc::downgrade(installed)))
        {
            return Err(Error::Stale);
        }
        resources::check_member_update(&state, caller, request.member)?;
        let entry = held.target_constructors.get(&open).ok_or(Error::Stale)?;
        if entry.work.worker.load(Ordering::Acquire) {
            return Err(Error::Busy);
        }
        if entry.preload.as_deref() != Some(request.operation)
            || entry.work.member != request.member
            || !entry.connection.same(connection)
            || entry.work.scope.cancelled()
            || state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !entry.work.scope.cancellation().owned_by(app))
        {
            return Err(Error::Stale);
        }
        let command = TargetConstruction {
            state: Arc::downgrade(&self.state),
            cell: Arc::downgrade(&cell),
            installed: Arc::downgrade(installed),
            kernel,
            work: entry.work.clone(),
            connection: connection.clone(),
            process,
            preload: Some(request.operation.into()),
            preload_attempt: Some(attempt.clone()),
        };
        // The actual retained constructor already counts before visible Waiting.
        state
            .preload_sequences
            .get_mut(&key)
            .ok_or(Error::Stale)?
            .begin_resample(&attempt, request.previous)?;
        drop(held);
        Ok(TargetResamplePreparation::Pending(Box::new(
            TargetResampleCommand {
                command,
                previous: request.previous.clone(),
            },
        )))
    }

    pub fn run_target_resample(
        &self,
        env: &Env,
        resample: Box<TargetResampleCommand>,
    ) -> Result<PreloadResample, Error> {
        let TargetResampleCommand { command, previous } = *resample;
        let state_arc = command.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state_arc, &self.state) {
            return Err(Error::Stale);
        }
        let attempt = command.preload_attempt.as_ref().ok_or(Error::Stale)?;
        let installed = command.installed.upgrade().ok_or(Error::Stale)?;
        let caller = installed.assignment.caller();
        let key = member_key(caller, &command.work.member)?;
        {
            let state = lock(&self.state)?;
            if let Some(result) = outcome(
                state
                    .preload_sequences
                    .get(&key)
                    .ok_or(Error::Stale)?
                    .resample_status(attempt, &previous)?,
                &previous,
            ) {
                return Ok(result);
            }
        }
        command
            .work
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        let _worker = ConstructorWorker(command.work.clone());
        let old = {
            let state = lock(&self.state)?;
            let member = state.members.get(&key).ok_or(Error::Stale)?;
            if member.operation != attempt.operation
                || member.registration != previous.registration
                || member.source.credential_generation() != previous.credential_generation
                || member.source.digest() != previous.digest
            {
                return Err(Error::Stale);
            }
            (member.source.clone(), member.readiness.reader_owned)
        };
        let permission = InstalledRead {
            command: &command,
            installed: &installed,
            expected: &old.0,
        };
        permission.current().map_err(|_| Error::Stale)?;
        let (source, receipt) =
            slot::preload_installed(env, &permission).map_err(|_| Error::Unavailable)?;
        permission.current().map_err(|_| Error::Stale)?;
        let source = Arc::new(source);
        let readiness = SourceReadiness::target(installed.resource.clone(), old.1)?;
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let result = Preload {
            member: command.work.member.clone(),
            operation: attempt.operation.clone(),
            registration,
            source,
        };
        let retired = {
            let mut state = lock(&self.state)?;
            let cell = command.cell.upgrade().ok_or(Error::Stale)?;
            same_cell(&state, &cell, caller)?;
            let mut held = lock(&cell)?;
            reservation_cell_scope(&state, &held)?;
            if !matches!(&held.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, &command.kernel))
                || !held
                    .installed
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &installed))
                || command.work.scope.cancelled()
                || !command.connection.is_live()
            {
                return Err(Error::Stale);
            }
            let entry = held
                .target_constructors
                .get(&command.work.operation)
                .ok_or(Error::Stale)?;
            if !Arc::ptr_eq(&entry.work, &command.work)
                || !entry.connection.same(&command.connection)
            {
                return Err(Error::Stale);
            }
            resources::check_member_update(&state, caller, &command.work.member)?;
            let prior = state.members.get(&key).ok_or(Error::Stale)?;
            if prior.registration != previous.registration
                || !Arc::ptr_eq(&prior.source, &old.0)
                || !prior.connection.same(&command.connection)
                || !same_source_scope(&prior.source, &result.source)
            {
                return Err(Error::Stale);
            }
            let mut ledger = state
                .preload_sequences
                .get(&key)
                .ok_or(Error::Stale)?
                .clone();
            ledger.finish_resample(attempt, &previous, result.clone())?;
            let version = version(&mut state)?;
            // The fresh registration permits one new immutable READY report.
            held.target_constructors
                .get_mut(&command.work.operation)
                .ok_or(Error::Stale)?
                .ready = None;
            state.preload_sequences.insert(key.clone(), ledger);
            state.members.insert(
                key,
                Member {
                    caller: caller.clone(),
                    process: command.process.clone(),
                    operation: attempt.operation.clone(),
                    registration: result.registration.clone(),
                    connection: command.connection.clone(),
                    version,
                    source: result.source.clone(),
                    readiness,
                    unknown: false,
                    work_rebound: false,
                },
            )
        };
        drop(retired);
        drop(receipt);
        Ok(PreloadResample::Preload(result))
    }
}
