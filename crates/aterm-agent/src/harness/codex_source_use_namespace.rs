// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A real admitted use may finish its own immutable source while assignment
//! admission is closed. IDs, metadata, or a pending phase alone grant nothing.
use super::*;
#[path = "codex_source_contextual_refresh.rs"]
mod contextual_refresh;
use crate::harness::codex_assignment as assignment;
pub use contextual_refresh::OwnedUseRefresh;

enum BookMode {
    Settled,
    Normalized {
        target: Arc<target::normalize::NormalizedTarget>,
    },
    // Cancelling the proposed switch does not revoke an independently admitted
    // A use. Its own Scope must remain live and the claimed Book must still be
    // exact A with no commit effect; this is not post-effect target authority.
    Origin {
        cell: Weak<Mutex<Cell>>,
        intent: Box<assignment::prepare::TargetIntent>,
    },
    Installed {
        cell: Weak<Mutex<Cell>>,
        installed: Weak<target::Installed>,
        commit: Arc<assignment::AssignmentCommit>,
        resource: Arc<crate::harness::codex_source_gate::TargetFamilyLease>,
    },
}
pub(in crate::harness) struct UseNamespace {
    role: ReadyRole,
    context: SourceUse,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    process: Process,
    connection: Connection,
    scope: crate::harness::account_work::Scope,
    source_lease: Option<Arc<SourceWorkLease>>,
    mode: BookMode,
}
impl std::fmt::Debug for UseNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UseNamespace(redacted)")
    }
}

pub(super) fn admitted<'a>(
    state: &'a State,
    context: &SourceUse,
) -> Result<(&'a ActiveUse, &'a Member), Error> {
    let key = member_key(&context.caller, &context.member)?;
    let uses = state.uses.get(&key).ok_or(Error::Stale)?;
    if uses.prefix != context.nonce.prefix {
        return Err(Error::Stale);
    }
    let active = uses
        .active
        .get(&context.nonce.counter)
        .ok_or(Error::Stale)?;
    let member = state.members.get(&key).ok_or(Error::Stale)?;
    if active.capability != context.capability
        || member.caller != context.caller
        || member.unknown
        || !member.connection.is_live()
        || !same_source_scope(&active.source, &member.source)
        || active.gate != member.readiness.identity
        || member.readiness.role == Some(ReadyRole::PolicyUnavailable)
        || active._scope.cancelled()
        || state
            .app_lifetime
            .as_ref()
            .is_none_or(|app| !active._scope.cancellation().owned_by(app))
    {
        return Err(Error::Stale);
    }
    // Ready may temporarily be reset by this same scope's admitted preload.
    // This is continuation of an existing capability, never Begin/new work.
    reservation_allows_member(state, &context.caller, &context.member)?;
    Ok((active, member))
}

fn mode_snapshot(
    state: &State,
    caller: &slot::Caller,
    member: &Member,
    source: &slot::SourceHandle,
    issued: &Issued,
) -> Result<(BookMode, Option<Arc<SourceWorkLease>>), Error> {
    let source_lease = match &member.readiness.gate {
        Some(ReaderGate::Global(lease)) => Some(lease.clone()),
        Some(ReaderGate::Target(resource)) => Some(resource.retained_source_lease()),
        None => None,
    };
    mode_for_source(state, caller, source, issued, source_lease)
}

fn mode_for_source(
    state: &State,
    caller: &slot::Caller,
    source: &slot::SourceHandle,
    issued: &Issued,
    mut source_lease: Option<Arc<SourceWorkLease>>,
) -> Result<(BookMode, Option<Arc<SourceWorkLease>>), Error> {
    let historical = issued
        .target
        .as_ref()
        .map(|target| target.upgrade().ok_or(Error::Stale))
        .transpose()?;
    let normalized = historical
        .as_ref()
        .and_then(|installed| target::normalize::lookup(state, caller, installed));
    if let Some(normalized) = &normalized {
        source_lease = Some(normalized.source_lease());
    }
    let origin = if let Some(cell) = state.reservations.get(&actor_key(caller)) {
        let held = lock(cell)?;
        if held.phase.owes_origin() && same_source_scope(held.origin.source(), source) {
            Some(BookMode::Origin {
                cell: Arc::downgrade(cell),
                intent: Box::new(held.target.intent()),
            })
        } else {
            None
        }
    } else {
        None
    };
    let mode = if let Some(origin) = origin {
        origin
    } else if let Some(normalized) = normalized {
        BookMode::Normalized { target: normalized }
    } else if let Some(installed) = historical {
        let cell = state
            .reservations
            .get(&actor_key(caller))
            .ok_or(Error::Stale)?;
        let held = lock(cell)?;
        if !held
            .installed
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &installed))
            || !same_source_scope(installed.source(), source)
            || !matches!(&held.phase, seal::Phase::Sealed { .. })
        {
            return Err(Error::Stale);
        }
        source_lease = Some(installed.resource().retained_source_lease());
        BookMode::Installed {
            cell: Arc::downgrade(cell),
            installed: Arc::downgrade(&installed),
            commit: installed.assignment().clone(),
            resource: installed.resource().clone(),
        }
    } else {
        BookMode::Settled
    };
    Ok((mode, source_lease))
}

impl SourceUse {
    /// Validate the already admitted source-family namespace, independently
    /// of the descriptive Ready temporarily reset by its own PRELOAD.
    pub(super) fn continuation_current(&self) -> Result<(), Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let gate = {
            let state = lock(&state)?;
            admitted(&state, self)?.0.gate.clone()
        };
        let home = gate.existing_home().ok_or(Error::Stale)?;
        self.namespace(&self.caller, &home)?.current()
    }
    pub(in crate::harness) fn namespace(
        &self,
        caller: &slot::Caller,
        home: &std::path::Path,
    ) -> Result<UseNamespace, Error> {
        if caller != &self.caller {
            return Err(Error::Denied);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let proof = {
            let state = lock(&state)?;
            let (active, member) = admitted(&state, self)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            let issued = actor.issued.get(&self.member).ok_or(Error::Stale)?;
            if actor.process != member.process || !issued.connection.same(&member.connection) {
                return Err(Error::Stale);
            }
            let (mode, source_lease) =
                mode_snapshot(&state, caller, member, &active.source, issued)?;
            UseNamespace {
                role: active.role,
                context: SourceUse {
                    state: self.state.clone(),
                    caller: self.caller.clone(),
                    member: self.member.clone(),
                    nonce: self.nonce,
                    capability: self.capability.clone(),
                },
                source: active.source.clone(),
                gate: active.gate.clone(),
                process: member.process.clone(),
                connection: member.connection.clone(),
                scope: active._scope.clone(),
                source_lease,
                mode,
            }
        };
        if !proof.gate.matches_home(home) {
            return Err(Error::Stale);
        }
        proof.current()?;
        Ok(proof)
    }
}

impl Registry {
    /// Resolve an already admitted capability while its same-source PRELOAD
    /// temporarily resets descriptive Ready. This admits no new Begin and
    /// retains the exact original connection/process/App/source namespace.
    pub(in crate::harness) fn continuing_use_context(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        nonce: UseNonce,
        capability: &str,
    ) -> Result<SourceUse, Error> {
        if !peer.current() || !connection.is_live() || !hex(capability, 64) {
            return Err(Error::Stale);
        }
        let context = SourceUse {
            state: Arc::downgrade(&self.state),
            caller: peer.caller.clone(),
            member: member.into(),
            nonce,
            capability: capability.into(),
        };
        let gate = {
            let state = lock(&self.state)?;
            issued_on(&state, peer, member, connection)?;
            let (active, loaded) = admitted(&state, &context)?;
            if loaded.process != peer.process || !loaded.connection.same(connection) {
                return Err(Error::Stale);
            }
            active.gate.clone()
        };
        let home = gate.existing_home().ok_or(Error::Stale)?;
        let namespace = context.namespace(&peer.caller, &home)?;
        if !namespace.connection.same(connection) {
            return Err(Error::Stale);
        }
        namespace.current()?;
        Ok(context)
    }
}

impl UseNamespace {
    pub(in crate::harness) fn refresh_allowed(&self) -> Result<(), Error> {
        self.current()?;
        match self.role {
            ReadyRole::Manager(_) => Ok(()),
            ReadyRole::ReaderLoaded | ReadyRole::PolicyUnavailable => Err(Error::Denied),
        }
    }
    pub(in crate::harness) fn revalidated(&self, home: &std::path::Path) -> Result<Self, Error> {
        let mut fresh = self.context.namespace(self.caller(), home)?;
        if fresh.process != self.process
            || fresh.scope.operation_identity() != self.scope.operation_identity()
            || !same_source_scope(&fresh.source, &self.source)
            || !revision_descends(
                fresh.source.credential_generation(),
                fresh.source.digest(),
                self.source.credential_generation(),
                self.source.digest(),
            )
        {
            return Err(Error::Stale);
        }
        // Retain the same actual claim across a legitimate reserve/normalizer
        // transition; no shared reacquisition behind a publication writer.
        if let Some(lease) = &self.source_lease {
            lease.current().map_err(|_| Error::Stale)?;
            fresh.source_lease = Some(lease.clone());
        }
        Ok(fresh)
    }
    pub(in crate::harness) fn revalidated_from_gate(&self) -> Result<Self, Error> {
        let home = self.gate.existing_home().ok_or(Error::Stale)?;
        self.revalidated(&home)
    }
    pub(in crate::harness) fn caller(&self) -> &slot::Caller {
        &self.context.caller
    }
    pub(in crate::harness) fn source(&self) -> &slot::SourceHandle {
        &self.source
    }
    pub(in crate::harness) fn scope(&self) -> &crate::harness::account_work::Scope {
        &self.scope
    }
    pub(in crate::harness) fn origin(&self) -> Option<&assignment::prepare::TargetIntent> {
        match &self.mode {
            BookMode::Origin { intent, .. } => Some(intent),
            _ => None,
        }
    }
    pub(in crate::harness) fn matches_home(&self, home: &std::path::Path) -> bool {
        self.gate.matches_home(home)
    }
    pub(in crate::harness) fn current(&self) -> Result<(), Error> {
        // Physical/process probes are outside the Registry mutex.
        if self.scope.cancelled()
            || !self.gate.current()
            || Process::observe(self.process.pid).as_ref() != Some(&self.process)
        {
            return Err(Error::Stale);
        }
        if let Some(lease) = &self.source_lease {
            lease.current().map_err(|_| Error::Stale)?;
        }
        if let BookMode::Installed { resource, .. } = &self.mode {
            resource.current().map_err(|_| Error::Stale)?;
        }
        if let BookMode::Normalized { target } = &self.mode {
            target.current()?;
        }
        let state = self.context.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let (active, member) = admitted(&state, &self.context)?;
        if active.role != self.role
            || !Arc::ptr_eq(&active.source, &self.source)
            || active.gate != self.gate
            || active._scope.operation_identity() != self.scope.operation_identity()
            || member.process != self.process
            || !member.connection.same(&self.connection)
        {
            return Err(Error::Stale);
        }
        match &self.mode {
            BookMode::Settled => {
                if state
                    .actors
                    .get(&actor_key(self.caller()))
                    .and_then(|actor| actor.issued.get(&self.context.member))
                    .is_none_or(|issued| issued.target.is_some())
                {
                    return Err(Error::Stale);
                }
            }
            BookMode::Normalized { target: normalized } => {
                let current =
                    target::normalize::lookup(&state, self.caller(), normalized.installed())
                        .ok_or(Error::Stale)?;
                if !Arc::ptr_eq(&current, normalized)
                    || !same_source_scope(normalized.installed().source(), &self.source)
                {
                    return Err(Error::Stale);
                }
            }
            BookMode::Origin { cell, intent } => {
                let cell = cell.upgrade().ok_or(Error::Stale)?;
                same_cell(&state, &cell, self.caller())?;
                let held = lock(&cell)?;
                if !held.phase.owes_origin()
                    || !held.target.matches_intent(intent)
                    || !same_source_scope(held.origin.source(), &self.source)
                {
                    return Err(Error::Stale);
                }
            }
            BookMode::Installed {
                cell,
                installed,
                resource,
                ..
            } => {
                let cell = cell.upgrade().ok_or(Error::Stale)?;
                let installed = installed.upgrade().ok_or(Error::Stale)?;
                same_cell(&state, &cell, self.caller())?;
                target::resources::check_member_update(
                    &state,
                    self.caller(),
                    &self.context.member,
                )?;
                let held = lock(&cell)?;
                reservation_cell_scope(&state, &held)?;
                if !held
                    .installed
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &installed))
                    || !Arc::ptr_eq(installed.resource(), resource)
                    || !matches!(&held.phase, seal::Phase::Sealed { .. })
                {
                    return Err(Error::Stale);
                }
            }
        }
        Ok(())
    }
    pub(in crate::harness) fn source_lease(&self) -> Result<Option<Arc<SourceWorkLease>>, String> {
        self.current().map_err(|_| "source use namespace changed")?;
        Ok(self.source_lease.clone())
    }
    pub(in crate::harness) fn with_namespace<R>(
        &self,
        env: &Env,
        run: impl FnOnce(&assignment::BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        self.current().map_err(|_| "source use namespace changed")?;
        if !env
            .home
            .as_deref()
            .is_some_and(|home| self.matches_home(home))
        {
            return Err("source use home changed".into());
        }
        let inspect = |namespace: &assignment::BookNamespace| {
            if self
                .source
                .namespaces()
                .is_none_or(|expected| expected.book() != namespace)
            {
                return Err("source Book namespace changed".into());
            }
            self.current().map_err(|_| "source use namespace changed")?;
            let result = run(namespace)?;
            self.current().map_err(|_| "source use namespace changed")?;
            Ok(result)
        };
        match &self.mode {
            BookMode::Settled | BookMode::Normalized { .. } => {
                assignment::with_current_assignment_namespace(
                    env,
                    self.caller(),
                    self.source.binding(),
                    self.source.assignment_generation(),
                    self.source.source_authority(),
                    inspect,
                )
            }
            BookMode::Origin { .. } => assignment::with_owed_source_namespace(env, self, inspect),
            BookMode::Installed { commit, .. } => commit.with_namespace(inspect),
        }
    }
}

#[path = "codex_source_preload_namespace.rs"]
mod preload;
pub(in crate::harness) use preload::PreloadReadNamespace;
pub use preload::{
    CapturedPreloadCommand, CapturedReadyCommand, CapturedReadyPreparation,
    OwnedPreloadPreparation, PreloadResample, PreloadUseIdentity, ReboundReadyCommand,
    ReboundReadyOutcome, ReboundReadyRequest, ResamplePreparation,
};
