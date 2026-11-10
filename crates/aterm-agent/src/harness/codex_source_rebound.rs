// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual same-source manager recovery after existing-only work rebind. Neither
//! a historical READY nor a new transport mapping is current read permission.
use super::*;

pub enum ReboundReadyOutcome {
    Ready,
    SourceChanged,
}

pub struct ReboundReadyRequest<'a> {
    pub member: &'a str,
    pub operation: &'a str,
    pub sequence: PreloadSequence,
    pub registration: &'a str,
    pub ready: Ready,
}

pub struct ReboundReadyCommand {
    command: CapturedPreloadCommand,
    registration: String,
    role: ReadyRole,
}
impl ReboundReadyCommand {
    pub fn run(mut self, env: &Env) -> Result<ReboundReadyOutcome, Error> {
        let source = match self.command.read_source(env)? {
            slot::SourceProgress::Ready(source) => source,
            slot::SourceProgress::Waiting => return Err(Error::Busy),
        };
        let proof = &self.command.proof;
        if !same_source_scope(&source, &proof.source) {
            return Err(Error::Stale);
        }
        proof.current()?;
        if source.credential_generation() > proof.source.credential_generation() {
            // Authenticated same-grant advancement is a negative observation,
            // not a Ready receipt or authority to use the new document.
            return Ok(ReboundReadyOutcome::SourceChanged);
        }
        if source.credential_generation() != proof.source.credential_generation()
            || source.digest() != proof.source.digest()
        {
            return Err(Error::Stale);
        }
        let registry = proof.state.upgrade().ok_or(Error::Stale)?;
        let key = member_key(&proof.caller, &proof.member)?;
        let mut state = lock(&registry)?;
        self.command.worker.check_completion(&state, &key)?;
        let loaded = state.members.get(&key).ok_or(Error::Stale)?;
        if loaded.registration != self.registration
            || loaded.process != proof.process
            || !loaded.connection.same(&proof.connection)
            || !Arc::ptr_eq(&loaded.source, &proof.source)
            || loaded.readiness.role != Some(self.role)
        {
            return Err(Error::Stale);
        }
        state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .rebound_completed_attempt(
                &proof.process,
                &proof.attempt.operation,
                proof.attempt.sequence,
                &self.registration,
                self.role,
            )?;
        state.members.get_mut(&key).ok_or(Error::Stale)?.unknown = false;
        let pending = state.preloads.remove(&key);
        drop(state);
        drop(pending);
        Ok(ReboundReadyOutcome::Ready)
    }
}

// Current native-issued membership, original OPEN and loaded manager source all
// participate. This does not reconstruct a Peer or relax any generic use gate.
fn manager_origin(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    connection: &Connection,
    member: &str,
) -> Result<(), Error> {
    if !connection.is_live() {
        return Err(Error::Stale);
    }
    let key = member_key(caller, member)?;
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    let issued = actor.issued.get(member).ok_or(Error::Stale)?;
    let loaded = state.members.get(&key).ok_or(Error::Stale)?;
    if actor.process != *process
        || loaded.process != *process
        || !issued.connection.same(connection)
        || !loaded.connection.same(connection)
        || !loaded.work_rebound
        || issued.target.is_some()
        || state.unrelated.contains_key(&key)
        || !matches!(loaded.readiness.role, Some(ReadyRole::Manager(_)))
    {
        return Err(Error::Stale);
    }
    reservation_allows_member(state, caller, member)?;
    fresh_ordinary_preload_allowed(state, caller)?;
    Ok(())
}

fn capture(
    registry: &Registry,
    state: &mut State,
    caller: &slot::Caller,
    process: &Process,
    connection: &Connection,
    member: &str,
    attempt: &preload_sequence::Attempt,
) -> Result<
    (
        CapturedPreloadCommand,
        Option<Arc<preload_custody::Pending>>,
    ),
    Error,
> {
    let key = member_key(caller, member)?;
    let loaded = state.members.get(&key).ok_or(Error::Stale)?;
    if let Some(pending) = state.preloads.get(&key) {
        if pending.operation == attempt.operation && pending.connection.same(connection) {
            return retain_existing(
                registry, state, caller, process, member, connection, attempt,
            )
            .map(|command| (command, None));
        }
        if pending.worker.load(Ordering::Acquire) || !pending.connection.same(connection) {
            return Err(Error::Busy);
        }
    }
    if !loaded.work_rebound {
        return Err(Error::Stale);
    }
    let issued = state
        .actors
        .get(&actor_key(caller))
        .and_then(|actor| actor.issued.get(member))
        .ok_or(Error::Stale)?;
    let source = loaded.source.clone();
    let gate = loaded.readiness.identity.clone();
    let (_, lease) = mode_snapshot(state, caller, loaded, &source, issued)?;
    let scope = state
        .app_lifetime
        .as_ref()
        .and_then(|app| app.enter())
        .ok_or(Error::Busy)?;
    preload_sequence::accept(state, &key, process, attempt, &scope)?;
    let worker_scope = scope.clone();
    let pending = Arc::new(preload_custody::Pending {
        operation: attempt.operation.clone(),
        connection: connection.clone(),
        worker: false.into(),
        revoked: false.into(),
        read_namespace: Mutex::new(None),
        retirement: scope.cancellation(),
        scope: Mutex::new(Some(scope)),
    });
    let proof = Arc::new(PreloadReadNamespace {
        state: Arc::downgrade(&registry.state),
        caller: caller.clone(),
        process: process.clone(),
        member: member.into(),
        connection: connection.clone(),
        attempt: attempt.clone(),
        source,
        gate,
        source_lease: lease,
        pending: Arc::downgrade(&pending),
        rebound: true,
    });
    *lock(&pending.read_namespace)? = Some(proof.clone());
    pending.worker.store(true, Ordering::Release);
    let command = CapturedPreloadCommand {
        proof,
        read_gate: None,
        worker: preload_custody::Worker {
            pending: pending.clone(),
            _scope: worker_scope,
        },
    };
    // All previous workers were checked retired. Return the replaced Pending to
    // the caller for dropping outside State, rather than releasing a Scope here.
    // No fallible operation follows replacement. The prior Pending's release
    // objects must cross the State boundary even on the caller's error path.
    let previous = state.preloads.insert(key, pending);
    Ok((command, previous))
}

impl Registry {
    pub fn prepare_rebound_ready(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: ReboundReadyRequest<'_>,
    ) -> Result<Box<ReboundReadyCommand>, Error> {
        let ReboundReadyRequest {
            member,
            operation,
            sequence,
            registration,
            ready,
        } = request;
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        manager_origin(&state, caller, &process, connection, member)?;
        let key = member_key(caller, member)?;
        let loaded = state.members.get(&key).ok_or(Error::Stale)?;
        let role = target::reported_role(&ready, &loaded.source)?;
        if !matches!(role, ReadyRole::Manager(_)) {
            return Err(Error::Denied);
        }
        let attempt = state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .rebound_completed_attempt(&process, operation, sequence, registration, role)?;
        let (command, previous) = capture(
            self, &mut state, caller, &process, connection, member, &attempt,
        )?;
        drop(state);
        drop(previous);
        command.current()?;
        Ok(Box::new(ReboundReadyCommand {
            command,
            registration: registration.into(),
            role,
        }))
    }
    pub fn prepare_rebound_preload(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
    ) -> Result<Box<CapturedPreloadCommand>, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        let key = member_key(caller, member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Ordinary,
        };
        // Exact accepted PRELOAD can finish after its publication reset Ready.
        let accepted = state
            .members
            .get(&key)
            .is_some_and(|loaded| loaded.work_rebound)
            && state.preloads.get(&key).is_some_and(|pending| {
                pending.operation == operation
                    && pending.connection.same(connection)
                    && pending
                        .read_namespace
                        .lock()
                        .is_ok_and(|proof| proof.as_ref().is_some_and(|proof| proof.rebound))
            });
        if !accepted {
            manager_origin(&state, caller, &process, connection, member)?;
        }
        let (command, previous) = capture(
            self, &mut state, caller, &process, connection, member, &attempt,
        )?;
        drop(state);
        drop(previous);
        command.current()?;
        Ok(Box::new(command))
    }
}
