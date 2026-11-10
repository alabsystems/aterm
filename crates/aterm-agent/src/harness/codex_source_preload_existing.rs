// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Completion of ordinary accepted reads and ordinary B reads after Applied.
//! Both keep their own original Scope; neither borrows an A use capability.
use super::*;

pub enum OwnedPreloadPreparation {
    Pending(Box<CapturedPreloadCommand>),
    Settled(Preload),
}
pub enum ResamplePreparation {
    Pending(Box<CapturedPreloadCommand>),
    Settled(PreloadResample),
}

fn retain_existing(
    registry: &Registry,
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    member: &str,
    connection: &Connection,
    attempt: &preload_sequence::Attempt,
) -> Result<CapturedPreloadCommand, Error> {
    let key = member_key(caller, member)?;
    let pending = state.preloads.get(&key).ok_or(Error::Stale)?.clone();
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    let issued = actor.issued.get(member).ok_or(Error::Stale)?;
    if actor.process != *process
        || !issued.connection.same(connection)
        || !pending.connection.same(connection)
        || pending.operation != attempt.operation
        || !connection.is_live()
        || pending.revoked.load(Ordering::Acquire)
    {
        return Err(Error::Stale);
    }
    reservation_allows_member(state, caller, member)?;
    preload_sequence::check_current(state, &key, process, attempt)?;
    let scope = lock(&pending.scope)?
        .as_ref()
        .cloned()
        .ok_or(Error::Stale)?;
    if scope.cancelled()
        || state
            .app_lifetime
            .as_ref()
            .is_none_or(|app| !scope.cancellation().owned_by(app))
    {
        return Err(Error::Stale);
    }
    let mut proof = lock(&pending.read_namespace)?;
    if proof.is_none() {
        let loaded = state.members.get(&key).ok_or(Error::Stale)?;
        if loaded.process != *process
            || loaded.operation != attempt.operation
            || !loaded.connection.same(connection)
        {
            return Err(Error::Stale);
        }
        let (_, lease) = mode_snapshot(state, caller, loaded, &loaded.source, issued)?;
        *proof = Some(Arc::new(PreloadReadNamespace {
            state: Arc::downgrade(&registry.state),
            caller: caller.clone(),
            process: process.clone(),
            member: member.into(),
            connection: connection.clone(),
            attempt: attempt.clone(),
            source: loaded.source.clone(),
            gate: loaded.readiness.identity.clone(),
            source_lease: lease,
            pending: Arc::downgrade(&pending),
            rebound: loaded.unknown,
        }));
    }
    let captured = proof.as_ref().ok_or(Error::Stale)?;
    if captured.attempt != *attempt || !captured.connection.same(connection) {
        return Err(Error::Stale);
    }
    pending
        .worker
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Error::Busy)?;
    let captured = captured.clone();
    drop(proof);
    Ok(CapturedPreloadCommand {
        proof: captured,
        read_gate: None,
        worker: preload_custody::Worker {
            pending,
            _scope: scope,
        },
    })
}

impl Registry {
    /// Resume an already accepted ordinary PRELOAD. This never creates a Scope
    /// and does not require the foreground to return from an owed tool.
    pub fn prepare_ordinary_preload_completion(
        &self,
        caller: &slot::Caller,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        connection: &Connection,
    ) -> Result<CapturedPreloadCommand, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth || !hex(operation, 32) {
            return Err(Error::Denied);
        }
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Ordinary,
        };
        let state = lock(&self.state)?;
        member_key(caller, member)?;
        if state
            .actors
            .get(&actor_key(caller))
            .and_then(|actor| actor.issued.get(member))
            .is_none_or(|issued| issued.target.is_some())
        {
            return Err(Error::Stale);
        }
        let command =
            retain_existing(self, &state, caller, &process, member, connection, &attempt)?;
        drop(state);
        command.current()?;
        Ok(command)
    }

    /// Resolve immutable resample/Ready metadata before looking for a worker;
    /// a new physical read can only retain the original accepted occurrence.
    pub fn prepare_ordinary_preload_resample(
        &self,
        caller: &slot::Caller,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        connection: &Connection,
        previous: &PreloadPredecessor,
    ) -> Result<ResamplePreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Denied);
        }
        let key = member_key(caller, member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Ordinary,
        };
        let state = lock(&self.state)?;
        if state
            .actors
            .get(&actor_key(caller))
            .is_none_or(|actor| actor.process != process)
        {
            return Err(Error::Stale);
        }
        match state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .resample_status(&attempt, previous)?
        {
            preload_sequence::ResampleAdmission::Replay(result) => {
                return Ok(ResamplePreparation::Settled(PreloadResample::Preload(
                    result,
                )));
            }
            preload_sequence::ResampleAdmission::ReadyCommitted => {
                return Ok(ResamplePreparation::Settled(
                    PreloadResample::ReadyCommitted {
                        previous: previous.clone(),
                    },
                ));
            }
            preload_sequence::ResampleAdmission::Read => (),
        }
        let command =
            retain_existing(self, &state, caller, &process, member, connection, &attempt)?;
        drop(state);
        command.current()?;
        Ok(ResamplePreparation::Pending(Box::new(command)))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the exact predecessor, original attempt and retained parent role are independent correlation inputs"
    )]
    pub fn prepare_contextual_preload_resample(
        &self,
        caller: &slot::Caller,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        connection: &Connection,
        previous: &PreloadPredecessor,
        parent: PreloadUseIdentity<'_>,
    ) -> Result<ResamplePreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth || !hex(parent.member, 32) || !hex(parent.capability, 64) {
            return Err(Error::Denied);
        }
        let key = member_key(caller, member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Use {
                member: parent.member.into(),
                nonce: format!("{:016x}{:016x}", parent.nonce.prefix, parent.nonce.counter),
                capability: parent.capability.into(),
            },
        };
        let state = lock(&self.state)?;
        if state
            .actors
            .get(&actor_key(caller))
            .is_none_or(|actor| actor.process != process)
        {
            return Err(Error::Stale);
        }
        match state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .resample_status(&attempt, previous)?
        {
            preload_sequence::ResampleAdmission::Replay(result) => {
                return Ok(ResamplePreparation::Settled(PreloadResample::Preload(
                    result,
                )));
            }
            preload_sequence::ResampleAdmission::ReadyCommitted => {
                return Ok(ResamplePreparation::Settled(
                    PreloadResample::ReadyCommitted {
                        previous: previous.clone(),
                    },
                ));
            }
            preload_sequence::ResampleAdmission::Read => (),
        }
        let command =
            retain_existing(self, &state, caller, &process, member, connection, &attempt)?;
        drop(state);
        command.current()?;
        Ok(ResamplePreparation::Pending(Box::new(command)))
    }

    /// Historical target routing is ordinary B authority only after the actual
    /// typed Applied normalizer exists. No A parent capability is consulted.
    pub fn prepare_normalized_preload(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: target::normalize::NormalizedMemberRequest<'_>,
        sequence: PreloadSequence,
    ) -> Result<OwnedPreloadPreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Denied);
        }
        let key = member_key(caller, request.member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: request.operation.into(),
            origin: preload_sequence::Origin::Target {
                assignment: request.assignment.into(),
                context: request.context.into(),
            },
        };
        let mut state = lock(&self.state)?;
        if state.preloads.contains_key(&key) {
            // The captured namespace will independently authenticate the actual
            // normalizer/current Book; lookup never reconstructs target privilege.
            let command = retain_existing(
                self,
                &state,
                caller,
                &process,
                request.member,
                connection,
                &attempt,
            )?;
            drop(state);
            command.current()?;
            return Ok(OwnedPreloadPreparation::Pending(Box::new(command)));
        }
        let (_, source, _) =
            target::normalize::selected_owned(&state, caller, &process, connection, &request)?;
        let loaded = state.members.get(&key).ok_or(Error::Stale)?;
        if loaded.operation == request.operation
            && state.preload_sequences.get(&key).is_some_and(|ledger| {
                ledger.check_attempt(&process, &attempt).is_ok()
                    && ledger.completed(&loaded.registration)
            })
        {
            let original = state
                .preload_sequences
                .get(&key)
                .and_then(|ledger| ledger.initial_preload(&attempt));
            return Ok(OwnedPreloadPreparation::Settled(original.unwrap_or(
                Preload {
                    member: request.member.into(),
                    operation: request.operation.into(),
                    registration: loaded.registration.clone(),
                    source,
                },
            )));
        }
        fresh_ordinary_preload_allowed(&state, caller)?;
        let scope = state
            .app_lifetime
            .as_ref()
            .and_then(|app| app.enter())
            .ok_or(Error::Busy)?;
        if let Err(error) = preload_sequence::accept(&mut state, &key, &process, &attempt, &scope) {
            drop(state);
            drop(scope);
            return Err(error);
        }
        if let Err(error) = target::normalize::note_owned_preload(
            &mut state,
            caller,
            request.member,
            request.operation,
        ) {
            drop(state);
            drop(scope);
            return Err(error);
        }
        let pending = Arc::new(preload_custody::Pending {
            operation: request.operation.into(),
            connection: connection.clone(),
            worker: false.into(),
            revoked: false.into(),
            read_namespace: Mutex::new(None),
            retirement: scope.cancellation(),
            scope: Mutex::new(Some(scope)),
        });
        state.preloads.insert(key, pending);
        // The prior loaded source is still exact B. retain_existing accepts
        // its new operation only after the actual normalized admission above.
        let command = retain_normalized_new(
            self,
            &state,
            caller,
            &process,
            request.member,
            connection,
            &attempt,
        )?;
        drop(state);
        command.current()?;
        Ok(OwnedPreloadPreparation::Pending(Box::new(command)))
    }
}

fn retain_normalized_new(
    registry: &Registry,
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    member: &str,
    connection: &Connection,
    attempt: &preload_sequence::Attempt,
) -> Result<CapturedPreloadCommand, Error> {
    let key = member_key(caller, member)?;
    let pending = state.preloads.get(&key).ok_or(Error::Stale)?;
    let loaded = state.members.get(&key).ok_or(Error::Stale)?;
    let issued = state
        .actors
        .get(&actor_key(caller))
        .and_then(|actor| actor.issued.get(member))
        .ok_or(Error::Stale)?;
    let (_, lease) = mode_snapshot(state, caller, loaded, &loaded.source, issued)?;
    *lock(&pending.read_namespace)? = Some(Arc::new(PreloadReadNamespace {
        state: Arc::downgrade(&registry.state),
        caller: caller.clone(),
        process: process.clone(),
        member: member.into(),
        connection: connection.clone(),
        attempt: attempt.clone(),
        source: loaded.source.clone(),
        gate: loaded.readiness.identity.clone(),
        source_lease: lease,
        pending: Arc::downgrade(pending),
        rebound: false,
    }));
    retain_existing(
        registry, state, caller, process, member, connection, attempt,
    )
}

pub enum CapturedReadyPreparation {
    Pending(Box<CapturedReadyCommand>),
    Settled,
}
pub struct CapturedReadyCommand {
    command: CapturedPreloadCommand,
    registration: String,
    ready: Ready,
}
impl CapturedReadyCommand {
    pub fn run(self, env: &Env) -> Result<(), Error> {
        let Self {
            mut command,
            registration,
            ready,
        } = self;
        command.ready(env, &registration, ready)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the exact original report and optional target generation are independent correlation fields"
)]
fn recorded_report(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    member: &str,
    attempt: &preload_sequence::Attempt,
    registration: &str,
    ready: &Ready,
    generation: Option<u64>,
) -> Result<bool, Error> {
    let key = member_key(caller, member)?;
    if state
        .actors
        .get(&actor_key(caller))
        .is_none_or(|actor| actor.process != *process)
    {
        return Err(Error::Stale);
    }
    let loaded = state.members.get(&key).ok_or(Error::Stale)?;
    if loaded.process != *process
        || loaded.operation != attempt.operation
        || loaded.registration != registration
        || generation
            .is_some_and(|value| value == 0 || value != loaded.source.assignment_generation())
    {
        return Err(Error::Stale);
    }
    let role = target::reported_role(ready, &loaded.source)?;
    let ledger = state.preload_sequences.get(&key).ok_or(Error::Stale)?;
    ledger.check_attempt(process, attempt)?;
    Ok(ledger.completed(registration) && loaded.readiness.role == Some(role))
}
#[expect(
    clippy::too_many_arguments,
    reason = "counted original read and immutable READY correlation remain explicit"
)]
fn prepare_report(
    registry: &Registry,
    caller: &slot::Caller,
    process: &Process,
    connection: &Connection,
    member: &str,
    attempt: preload_sequence::Attempt,
    registration: &str,
    ready: Ready,
    generation: Option<u64>,
) -> Result<CapturedReadyPreparation, Error> {
    let state = lock(&registry.state)?;
    if recorded_report(
        &state,
        caller,
        process,
        member,
        &attempt,
        registration,
        &ready,
        generation,
    )? {
        return Ok(CapturedReadyPreparation::Settled);
    }
    let command = retain_existing(
        registry, &state, caller, process, member, connection, &attempt,
    )?;
    drop(state);
    command.capture_ready_report(registration, &ready)?;
    command.current()?;
    Ok(CapturedReadyPreparation::Pending(Box::new(
        CapturedReadyCommand {
            command,
            registration: registration.into(),
            ready,
        },
    )))
}
impl Registry {
    #[expect(
        clippy::too_many_arguments,
        reason = "immutable READY report is correlated with the exact original attempt before Waiting"
    )]
    pub fn prepare_ordinary_ready(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        registration: &str,
        ready: Ready,
    ) -> Result<CapturedReadyPreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Denied);
        }
        prepare_report(
            self,
            caller,
            &process,
            connection,
            member,
            preload_sequence::Attempt {
                sequence,
                operation: operation.into(),
                origin: preload_sequence::Origin::Ordinary,
            },
            registration,
            ready,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the retained parent role and immutable READY attempt are distinct correlation bodies"
    )]
    pub fn prepare_preload_use_ready(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        parent: PreloadUseIdentity<'_>,
        registration: &str,
        ready: Ready,
    ) -> Result<CapturedReadyPreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth || !hex(parent.member, 32) || !hex(parent.capability, 64) {
            return Err(Error::Denied);
        }
        prepare_report(
            self,
            caller,
            &process,
            connection,
            member,
            preload_sequence::Attempt {
                sequence,
                operation: operation.into(),
                origin: preload_sequence::Origin::Use {
                    member: parent.member.into(),
                    nonce: format!("{:016x}{:016x}", parent.nonce.prefix, parent.nonce.counter),
                    capability: parent.capability.into(),
                },
            },
            registration,
            ready,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the original target context and actual generation remain explicit alongside the exact READY"
    )]
    pub fn prepare_normalized_ready(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: target::normalize::NormalizedMemberRequest<'_>,
        sequence: PreloadSequence,
        generation: u64,
        registration: &str,
        ready: Ready,
    ) -> Result<CapturedReadyPreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Denied);
        }
        prepare_report(
            self,
            caller,
            &process,
            connection,
            request.member,
            preload_sequence::Attempt {
                sequence,
                operation: request.operation.into(),
                origin: preload_sequence::Origin::Target {
                    assignment: request.assignment.into(),
                    context: request.context.into(),
                },
            },
            registration,
            ready,
            Some(generation),
        )
    }
}

impl Registry {
    /// The target-tagged resample route after Applied retains the ordinary B
    /// PRELOAD occurrence; it never borrows the departed A constructor/use.
    pub fn prepare_normalized_resample(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: target::normalize::NormalizedMemberRequest<'_>,
        sequence: PreloadSequence,
        previous: &PreloadPredecessor,
    ) -> Result<ResamplePreparation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth
            || !hex(request.assignment, 32)
            || !hex(request.context, 64)
            || !hex(request.operation, 32)
        {
            return Err(Error::Denied);
        }
        let key = member_key(caller, request.member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: request.operation.into(),
            origin: preload_sequence::Origin::Target {
                assignment: request.assignment.into(),
                context: request.context.into(),
            },
        };
        let state = lock(&self.state)?;
        if state
            .actors
            .get(&actor_key(caller))
            .is_none_or(|actor| actor.process != process)
        {
            return Err(Error::Stale);
        }
        match state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .resample_status(&attempt, previous)?
        {
            preload_sequence::ResampleAdmission::Replay(result) => {
                return Ok(ResamplePreparation::Settled(PreloadResample::Preload(
                    result,
                )));
            }
            preload_sequence::ResampleAdmission::ReadyCommitted => {
                return Ok(ResamplePreparation::Settled(
                    PreloadResample::ReadyCommitted {
                        previous: previous.clone(),
                    },
                ));
            }
            preload_sequence::ResampleAdmission::Read => (),
        }
        // A pre-ACK constructor is not a normalized Pending and cannot pass
        // this producer. Its separate installed-target resample owns that role.
        let command = retain_existing(
            self,
            &state,
            caller,
            &process,
            request.member,
            connection,
            &attempt,
        )?;
        drop(state);
        command.current()?;
        Ok(ResamplePreparation::Pending(Box::new(command)))
    }
}

#[path = "codex_source_rebound.rs"]
mod rebound;
pub use rebound::{ReboundReadyCommand, ReboundReadyOutcome, ReboundReadyRequest};
