// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only custody of an accepted contextual PRELOAD. The parent capability
//! is checked at admission, then retained only as immutable correlation data.
//! A captured occurrence cannot be passed to the provider refresh engine.
use super::*;

pub(in crate::harness) struct PreloadReadNamespace {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    member: String,
    connection: Connection,
    attempt: preload_sequence::Attempt,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    source_lease: Option<Arc<SourceWorkLease>>,
    pending: Weak<preload_custody::Pending>,
    rebound: bool,
}

/// Owns the original counted PRELOAD worker. Dropping an observer does not
/// remove the Pending occurrence or turn an unknown result into retirement.
/// Request correlation data only. A first acceptance must join a real Active
/// capability; a later completion must join the exact retained PRELOAD body.
pub struct PreloadUseIdentity<'a> {
    pub member: &'a str,
    pub nonce: UseNonce,
    pub capability: &'a str,
}

pub enum PreloadResample {
    Preload(Preload),
    /// Correlation only. The SDK must replay its retained immutable READY.
    ReadyCommitted {
        previous: PreloadPredecessor,
    },
}

pub struct CapturedPreloadCommand {
    proof: Arc<PreloadReadNamespace>,
    read_gate: Option<Arc<SourceWorkLease>>,
    worker: preload_custody::Worker,
}
impl std::fmt::Debug for CapturedPreloadCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CapturedPreloadCommand(redacted)")
    }
}
impl CapturedPreloadCommand {
    /// Validate the original counted occurrence without admitting new work.
    pub fn current(&self) -> Result<(), Error> {
        self.proof.current()
    }
    /// Source discovery only; no provider exchange or new activity admission.
    pub fn read_source(&mut self, env: &Env) -> Result<slot::SourceProgress, Error> {
        self.proof.current()?;
        let home = env.home.as_deref().ok_or(Error::Unavailable)?;
        if !self.proof.gate.matches_home(home) {
            return Err(Error::Stale);
        }
        let gate = match &self.proof.source_lease {
            Some(gate) => gate.clone(),
            None => Arc::new(SourceWorkLease::for_home(home).map_err(|_| Error::Busy)?),
        };
        if gate.identity().map_err(|_| Error::Stale)? != self.proof.gate {
            return Err(Error::Stale);
        }
        let result = slot::source_for_preload(
            env,
            &self.proof.caller,
            &self.proof.attempt.operation,
            &self.proof,
        )?;
        self.proof.current()?;
        gate.current().map_err(|_| Error::Stale)?;
        self.read_gate = Some(gate);
        Ok(result)
    }
    /// Publish one actual registration for this PRELOAD. Replaying its initial
    /// request returns the recorded snapshot; advancing it requires resample.
    pub fn preload(&mut self, env: &Env) -> Result<Preload, Error> {
        self.proof.current()?;
        let state_arc = self.proof.state.upgrade().ok_or(Error::Stale)?;
        let key = member_key(&self.proof.caller, &self.proof.member)?;
        {
            let state = lock(&state_arc)?;
            self.worker.check_completion(&state, &key)?;
            if let Some(result) = state
                .preload_sequences
                .get(&key)
                .and_then(|ledger| ledger.initial_preload(&self.proof.attempt))
            {
                return Ok(result);
            }
        }
        let source = match self.read_source(env)? {
            slot::SourceProgress::Ready(source) => Arc::new(source),
            slot::SourceProgress::Waiting => return Err(Error::Busy),
        };
        self.publish(source, None)
    }
    pub fn resample(
        &mut self,
        env: &Env,
        previous: &PreloadPredecessor,
    ) -> Result<PreloadResample, Error> {
        let state_arc = self.proof.state.upgrade().ok_or(Error::Stale)?;
        let key = member_key(&self.proof.caller, &self.proof.member)?;
        let admission = {
            let state = lock(&state_arc)?;
            state
                .preload_sequences
                .get(&key)
                .ok_or(Error::Stale)?
                .resample_status(&self.proof.attempt, previous)?
        };
        match admission {
            preload_sequence::ResampleAdmission::Replay(result) => {
                return Ok(PreloadResample::Preload(result));
            }
            preload_sequence::ResampleAdmission::ReadyCommitted => {
                return Ok(PreloadResample::ReadyCommitted {
                    previous: previous.clone(),
                });
            }
            preload_sequence::ResampleAdmission::Read => (),
        }
        self.proof.current()?;
        {
            let mut state = lock(&state_arc)?;
            self.worker.check_completion(&state, &key)?;
            match state
                .preload_sequences
                .get_mut(&key)
                .ok_or(Error::Stale)?
                .begin_resample(&self.proof.attempt, previous)?
            {
                preload_sequence::ResampleAdmission::Read => (),
                _ => return Err(Error::Busy),
            }
        }
        let source = match self.read_source(env)? {
            slot::SourceProgress::Ready(source) => Arc::new(source),
            slot::SourceProgress::Waiting => return Err(Error::Busy),
        };
        self.publish(source, Some(previous))
            .map(PreloadResample::Preload)
    }
    fn publish(
        &mut self,
        source: Arc<slot::SourceHandle>,
        previous_step: Option<&PreloadPredecessor>,
    ) -> Result<Preload, Error> {
        let state_arc = self.proof.state.upgrade().ok_or(Error::Stale)?;
        let key = member_key(&self.proof.caller, &self.proof.member)?;
        let mode = self.proof.snapshot()?;
        let target_resource = match mode {
            BookMode::Installed { resource, .. } => Some(resource),
            BookMode::Normalized { target } => Some(target.installed().resource().clone()),
            BookMode::Settled | BookMode::Origin { .. } => None,
        };
        if let Some(resource) = &target_resource {
            resource.current().map_err(|_| Error::Stale)?;
        }
        let gate = self.read_gate.as_ref().ok_or(Error::Stale)?.clone();
        gate.current().map_err(|_| Error::Stale)?;
        // Kernel checks precede State; the final transaction only joins the
        // exact metadata and transfers already authenticated custody.
        let mut readiness = match target_resource {
            Some(resource) => SourceReadiness::target(resource, false)?,
            None => SourceReadiness::preload_shared(gate, None)?,
        };
        let mut state = lock(&state_arc)?;
        self.worker.check_completion(&state, &key)?;
        preload_sequence::check_current(&state, &key, &self.proof.process, &self.proof.attempt)?;
        reservation_allows_member(&state, &self.proof.caller, &self.proof.member)?;
        let issued = state
            .actors
            .get(&actor_key(&self.proof.caller))
            .filter(|actor| actor.process == self.proof.process)
            .and_then(|actor| actor.issued.get(&self.proof.member))
            .ok_or(Error::Stale)?;
        if !issued.connection.same(&self.proof.connection)
            || !self.proof.connection.is_live()
            || !same_source_scope(&source, &self.proof.source)
            || !revision_descends(
                source.credential_generation(),
                source.digest(),
                self.proof.source.credential_generation(),
                self.proof.source.digest(),
            )
        {
            return Err(Error::Stale);
        }
        target::resources::check_member_update(&state, &self.proof.caller, &self.proof.member)?;
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let version = version(&mut state)?;
        let previous = state.members.get(&key).map(|member| &member.readiness);
        if previous.is_some_and(|previous| {
            previous.identity != readiness.identity
                || (previous.reader_owned && previous.gate.is_none())
        }) {
            return Err(Error::Stale);
        }
        let mut ledger = state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .clone();
        let result = Preload {
            member: self.proof.member.clone(),
            operation: self.proof.attempt.operation.clone(),
            registration: registration.clone(),
            source: source.clone(),
        };
        if let Some(previous) = previous_step {
            ledger.finish_resample(&self.proof.attempt, previous, result.clone())?;
        } else {
            ledger.loaded(
                &self.proof.attempt,
                &registration,
                source.credential_generation(),
                source.digest(),
            )?;
            ledger.record_initial(&self.proof.attempt, result.clone())?;
        }
        // No fallible operation follows taking the actual prior reader gate.
        if let Some(previous) = state
            .members
            .get_mut(&key)
            .map(|member| &mut member.readiness)
            && previous.reader_owned
        {
            readiness.reader_owned = true;
            readiness.gate = previous.gate.take();
        }
        state.preload_sequences.insert(key.clone(), ledger);
        let work_rebound = state
            .members
            .get(&key)
            .is_some_and(|member| member.work_rebound);
        let previous_member = state.members.insert(
            key,
            Member {
                caller: self.proof.caller.clone(),
                process: self.proof.process.clone(),
                operation: self.proof.attempt.operation.clone(),
                registration: registration.clone(),
                connection: self.proof.connection.clone(),
                version,
                source: source.clone(),
                readiness,
                unknown: false,
                work_rebound,
            },
        );
        drop(state);
        drop(previous_member);
        Ok(Preload {
            member: self.proof.member.clone(),
            operation: self.proof.attempt.operation.clone(),
            registration,
            source,
        })
    }
    fn capture_ready_report(&self, registration: &str, ready: &Ready) -> Result<(), Error> {
        let state_arc = self.proof.state.upgrade().ok_or(Error::Stale)?;
        let mut state = lock(&state_arc)?;
        let key = member_key(&self.proof.caller, &self.proof.member)?;
        self.worker.check_completion(&state, &key)?;
        let member = state.members.get(&key).ok_or(Error::Stale)?;
        if member.process != self.proof.process
            || member.operation != self.proof.attempt.operation
            || member.registration != registration
            || !member.connection.same(&self.proof.connection)
            || !same_source_scope(&member.source, &self.proof.source)
        {
            return Err(Error::Stale);
        }
        let role = target::reported_role(ready, &member.source)?;
        state
            .preload_sequences
            .get_mut(&key)
            .ok_or(Error::Stale)?
            .accept_ready(&self.proof.attempt, registration, role)
    }

    /// Complete only the actual PRELOAD's immutable report. Accepted metadata
    /// replay is resolved before requiring still-live work permission.
    pub fn ready(&mut self, env: &Env, registration: &str, ready: Ready) -> Result<(), Error> {
        let state_arc = self.proof.state.upgrade().ok_or(Error::Stale)?;
        let key = member_key(&self.proof.caller, &self.proof.member)?;
        let (expected, role) = {
            let mut state = lock(&state_arc)?;
            let member = state.members.get(&key).ok_or(Error::Stale)?;
            if member.process != self.proof.process
                || member.operation != self.proof.attempt.operation
                || member.registration != registration
                || !member.connection.same(&self.proof.connection)
            {
                return Err(Error::Stale);
            }
            let role = target::reported_role(&ready, &member.source)?;
            let expected = member.source.clone();
            if state.preload_sequences.get(&key).is_some_and(|ledger| {
                ledger.recorded_ready(&self.proof.attempt, registration, role)
            }) {
                return Ok(());
            }
            self.worker.check_completion(&state, &key)?;
            state
                .preload_sequences
                .get_mut(&key)
                .ok_or(Error::Stale)?
                .accept_ready(&self.proof.attempt, registration, role)?;
            (expected, role)
        };
        let now = match self.read_source(env)? {
            slot::SourceProgress::Ready(source) => source,
            slot::SourceProgress::Waiting => return Err(Error::Busy),
        };
        if !same_source_scope(&now, &expected)
            || now.credential_generation() != expected.credential_generation()
            || now.digest() != expected.digest()
        {
            return Err(Error::Stale);
        }
        self.proof.current()?;
        let mut state = lock(&state_arc)?;
        self.worker.check_completion(&state, &key)?;
        preload_sequence::check_current(&state, &key, &self.proof.process, &self.proof.attempt)?;
        reservation_allows_member(&state, &self.proof.caller, &self.proof.member)?;
        target::resources::check_member_update(&state, &self.proof.caller, &self.proof.member)?;
        let member = state.members.get(&key).ok_or(Error::Stale)?;
        if member.registration != registration
            || !Arc::ptr_eq(&member.source, &expected)
            || !member.connection.same(&self.proof.connection)
            || (member.unknown && !self.proof.rebound)
            || (member.readiness.reader_owned && role != ReadyRole::ReaderLoaded)
            || member
                .readiness
                .role
                .is_some_and(|previous| previous != role)
            || (role == ReadyRole::ReaderLoaded && member.readiness.gate.is_none())
        {
            return Err(Error::Stale);
        }
        let mut ledger = state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .clone();
        ledger.accept_ready(&self.proof.attempt, registration, role)?;
        ledger.complete(&self.proof.attempt, registration)?;
        let member = state.members.get_mut(&key).ok_or(Error::Stale)?;
        member.unknown = false;
        member.readiness.role = Some(role);
        member.readiness.reader_owned |= role == ReadyRole::ReaderLoaded;
        let released_gate = if role != ReadyRole::ReaderLoaded {
            member.readiness.gate.take()
        } else {
            None
        };
        state.preload_sequences.insert(key.clone(), ledger);
        let pending = state.preloads.remove(&key);
        drop(state);
        drop(released_gate);
        drop(pending);
        Ok(())
    }

    pub fn scope(&self) -> &crate::harness::account_work::Scope {
        self.worker.scope()
    }
}

impl Registry {
    /// Front-door version which does not require reconstructing an Active
    /// SourceUse after its parent ended. The raw body grants nothing by itself.
    pub fn prepare_preload_use(
        &self,
        caller: &slot::Caller,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        connection: &Connection,
        parent: PreloadUseIdentity<'_>,
    ) -> Result<CapturedPreloadCommand, Error> {
        if !hex(parent.member, 32) || !hex(parent.capability, 64) {
            return Err(Error::Denied);
        }
        let context = SourceUse {
            state: Arc::downgrade(&self.state),
            caller: caller.clone(),
            member: parent.member.into(),
            nonce: parent.nonce,
            capability: parent.capability.into(),
        };
        self.prepare_preload_for_use(caller, member, operation, sequence, connection, &context)
    }

    /// Pure replay of an actually recorded READY. No incoming connection,
    /// Scope, source load, active parent, or readiness grant is constructed.
    #[expect(
        clippy::too_many_arguments,
        reason = "the immutable READY and original PRELOAD role are independently correlated"
    )]
    pub fn replay_preload_use_ready(
        &self,
        caller: &slot::Caller,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        registration: &str,
        ready: &Ready,
        parent: PreloadUseIdentity<'_>,
    ) -> Result<bool, Error> {
        let observed = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if observed.birth != caller.birth
            || !hex(parent.member, 32)
            || !hex(parent.capability, 64)
            || !hex(operation, 32)
            || !hex(registration, 32)
        {
            return Err(Error::Denied);
        }
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
        let key = member_key(caller, member)?;
        if state
            .actors
            .get(&actor_key(caller))
            .is_none_or(|actor| actor.process != observed)
        {
            return Err(Error::Stale);
        }
        let current = state.members.get(&key).ok_or(Error::Stale)?;
        if current.process != observed
            || current.operation != operation
            || current.registration != registration
        {
            return Err(Error::Stale);
        }
        let role = target::reported_role(ready, &current.source)?;
        Ok(state
            .preload_sequences
            .get(&key)
            .is_some_and(|ledger| ledger.recorded_ready(&attempt, registration, role)))
    }

    /// Count the exact contextual PRELOAD before any source I/O. Exact Pending
    /// replay does not require a parent which has since ended; new acceptance
    /// always validates the real active parent and the accepted OPEN relation.
    pub fn prepare_preload_for_use(
        &self,
        caller: &slot::Caller,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
        connection: &Connection,
        context: &SourceUse,
    ) -> Result<CapturedPreloadCommand, Error> {
        if !hex(operation, 32) || caller != &context.caller {
            return Err(Error::Denied);
        }
        let key = member_key(caller, member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Use {
                member: context.member.clone(),
                nonce: format!(
                    "{:016x}{:016x}",
                    context.nonce.prefix, context.nonce.counter
                ),
                capability: context.capability.clone(),
            },
        };
        let state_arc = context.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state_arc, &self.state) {
            return Err(Error::Denied);
        }
        let mut state = lock(&self.state)?;
        let pending = if let Some(pending) = state.preloads.get(&key) {
            let proof = lock(&pending.read_namespace)?
                .as_ref()
                .cloned()
                .ok_or(Error::Stale)?;
            if proof.attempt != attempt || !proof.connection.same(connection) {
                return Err(Error::Stale);
            }
            preload_sequence::check_current(&state, &key, &proof.process, &attempt)?;
            pending.clone()
        } else {
            let (active, parent) = admitted(&state, context)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            let issued = actor.issued.get(member).ok_or(Error::Stale)?;
            if !connection.is_live()
                || !issued.connection.same(connection)
                || actor.process != parent.process
                || (member != context.member
                    && issued.parent_use.as_ref() != Some(&parent_use(context)))
                || state.unrelated.contains_key(&key)
                || state
                    .members
                    .get(&key)
                    .is_some_and(|member| !same_source_scope(&member.source, &active.source))
                || (issued.target.is_some()
                    && (issued.operation == operation || !state.members.contains_key(&key)))
            {
                return Err(Error::Stale);
            }
            reservation_allows_member(&state, caller, member)?;
            let target_member = issued.target.is_some();
            let (_, source_lease) = mode_snapshot(&state, caller, parent, &active.source, issued)?;
            let source = active.source.clone();
            let gate = active.gate.clone();
            let process = parent.process.clone();
            let scope = state
                .app_lifetime
                .as_ref()
                .and_then(|app| app.enter())
                .ok_or(Error::Busy)?;
            if let Err(error) =
                preload_sequence::accept(&mut state, &key, &process, &attempt, &scope)
            {
                drop(state);
                drop(scope);
                return Err(error);
            }
            if target_member
                && let Err(error) =
                    target::normalize::note_owned_preload(&mut state, caller, member, operation)
            {
                drop(state);
                drop(scope);
                return Err(error);
            }
            let pending = Arc::new_cyclic(|weak| preload_custody::Pending {
                operation: operation.into(),
                connection: connection.clone(),
                worker: false.into(),
                revoked: false.into(),
                read_namespace: Mutex::new(Some(Arc::new(PreloadReadNamespace {
                    state: Arc::downgrade(&self.state),
                    caller: caller.clone(),
                    process,
                    member: member.into(),
                    connection: connection.clone(),
                    attempt,
                    source,
                    gate,
                    source_lease,
                    pending: weak.clone(),
                    rebound: false,
                }))),
                retirement: scope.cancellation(),
                scope: Mutex::new(Some(scope)),
            });
            state.preloads.insert(key.clone(), pending.clone());
            pending
        };
        let scope = lock(&pending.scope)?
            .as_ref()
            .cloned()
            .ok_or(Error::Stale)?;
        if scope.cancelled() || pending.revoked.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        pending
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        let proof = lock(&pending.read_namespace)?
            .as_ref()
            .cloned()
            .ok_or(Error::Stale)?;
        drop(state);
        let command = CapturedPreloadCommand {
            proof,
            read_gate: None,
            worker: preload_custody::Worker {
                pending,
                _scope: scope,
            },
        };
        command.proof.current()?;
        Ok(command)
    }
}

impl PreloadReadNamespace {
    pub(in crate::harness::codex_source_registry) fn rebound(
        &self,
        pending: Weak<preload_custody::Pending>,
        connection: Connection,
    ) -> Self {
        Self {
            state: self.state.clone(),
            caller: self.caller.clone(),
            process: self.process.clone(),
            member: self.member.clone(),
            connection,
            attempt: self.attempt.clone(),
            source: self.source.clone(),
            gate: self.gate.clone(),
            source_lease: self.source_lease.clone(),
            pending,
            rebound: true,
        }
    }
    pub(in crate::harness) fn caller(&self) -> &slot::Caller {
        &self.caller
    }
    pub(in crate::harness) fn source(&self) -> &slot::SourceHandle {
        &self.source
    }

    fn snapshot(&self) -> Result<BookMode, Error> {
        // All physical and process probes happen outside State.
        if !self.gate.current()
            || Process::observe(self.process.pid).as_ref() != Some(&self.process)
        {
            return Err(Error::Stale);
        }
        if let Some(lease) = &self.source_lease {
            lease.current().map_err(|_| Error::Stale)?;
        }
        let pending = self.pending.upgrade().ok_or(Error::Stale)?;
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let key = member_key(&self.caller, &self.member)?;
        if !state
            .preloads
            .get(&key)
            .is_some_and(|value| Arc::ptr_eq(value, &pending))
            || pending.revoked.load(Ordering::Acquire)
            || !pending.worker.load(Ordering::Acquire)
            || !self.connection.is_live()
        {
            return Err(Error::Stale);
        }
        let scope = lock(&pending.scope)?;
        let scope = scope.as_ref().ok_or(Error::Stale)?;
        if scope.cancelled()
            || state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !scope.cancellation().owned_by(app))
        {
            return Err(Error::Stale);
        }
        preload_sequence::check_current(&state, &key, &self.process, &self.attempt)?;
        reservation_allows_member(&state, &self.caller, &self.member)?;
        let actor = state
            .actors
            .get(&actor_key(&self.caller))
            .ok_or(Error::Stale)?;
        let issued = actor.issued.get(&self.member).ok_or(Error::Stale)?;
        if actor.process != self.process
            || !issued.connection.same(&self.connection)
            || state.unrelated.contains_key(&key)
            || state.members.get(&key).is_some_and(|member| {
                (member.unknown && !self.rebound)
                    || member.process != self.process
                    || !same_source_scope(&member.source, &self.source)
            })
        {
            return Err(Error::Stale);
        }
        let (mode, _) = mode_for_source(
            &state,
            &self.caller,
            &self.source,
            issued,
            self.source_lease.clone(),
        )?;
        Ok(mode)
    }
    pub(in crate::harness) fn current(&self) -> Result<(), Error> {
        let mode = self.snapshot()?;
        match mode {
            BookMode::Normalized { target } => target.current()?,
            BookMode::Installed { resource, .. } => resource.current().map_err(|_| Error::Stale)?,
            BookMode::Settled | BookMode::Origin { .. } => (),
        }
        Ok(())
    }
    pub(in crate::harness) fn with_namespace<R>(
        &self,
        env: &Env,
        caller: &slot::Caller,
        binding: &crate::harness::codex_grant_owner::GrantBinding,
        generation: u64,
        authority: &assignment::SourceAuthority,
        run: impl FnOnce(&assignment::BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        if caller != &self.caller
            || binding != self.source.binding()
            || generation != self.source.assignment_generation()
            || authority != self.source.source_authority()
            || !env
                .home
                .as_deref()
                .is_some_and(|home| self.gate.matches_home(home))
        {
            return Err("preload namespace body changed".into());
        }
        let mode = self.snapshot().map_err(|_| "preload occurrence changed")?;
        let inspect = |namespace: &assignment::BookNamespace| {
            if self
                .source
                .namespaces()
                .is_none_or(|expected| expected.book() != namespace || !expected.current())
            {
                return Err("preload physical namespace changed".into());
            }
            self.current().map_err(|_| "preload occurrence changed")?;
            let result = run(namespace)?;
            self.current().map_err(|_| "preload occurrence changed")?;
            Ok(result)
        };
        match mode {
            BookMode::Settled | BookMode::Normalized { .. } => {
                assignment::with_current_assignment_namespace(
                    env, caller, binding, generation, authority, inspect,
                )
            }
            BookMode::Origin { intent, .. } => {
                assignment::with_owed_preload_namespace(env, self, &intent, inspect)
            }
            BookMode::Installed { commit, .. } => commit.with_namespace(inspect),
        }
    }
}

#[path = "codex_source_preload_existing.rs"]
mod existing;
pub use existing::{
    CapturedReadyCommand, CapturedReadyPreparation, OwnedPreloadPreparation, ReboundReadyCommand,
    ReboundReadyOutcome, ReboundReadyRequest, ResamplePreparation,
};
