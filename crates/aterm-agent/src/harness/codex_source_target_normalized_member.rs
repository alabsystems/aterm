// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Existing B members retain historical target routing after actual ACK. This
//! route has ordinary settled-source authority, never target-factory authority.
use super::*;

pub struct NormalizedMemberRequest<'a> {
    pub assignment: &'a str,
    pub context: &'a str,
    pub member: &'a str,
    pub operation: &'a str,
}

pub(in crate::harness) struct NormalizedRead<'a> {
    state: &'a Arc<Mutex<State>>,
    target: Arc<NormalizedTarget>,
    caller: &'a slot::Caller,
    process: &'a Process,
    connection: &'a Connection,
    member: &'a str,
    source: Arc<slot::SourceHandle>,
    registration: String,
    scope: &'a crate::harness::account_work::Scope,
}
impl NormalizedRead<'_> {
    pub(in crate::harness) fn caller(&self) -> &slot::Caller {
        self.caller
    }
    pub(in crate::harness) fn expected(&self) -> &slot::SourceHandle {
        &self.source
    }
    pub(in crate::harness) fn assignment(&self) -> &assignment::AssignmentCommit {
        &self.target.installed.assignment
    }
    pub(in crate::harness) fn gate(&self) -> &SourceGateIdentity {
        self.target.installed.resource.gate_identity()
    }
    fn matches(&self, state: &State) -> Result<(), Error> {
        if self.scope.cancelled()
            || !self.connection.is_live()
            || state
                .app_lifetime
                .as_ref()
                .is_none_or(|app| !self.scope.cancellation().owned_by(app))
        {
            return Err(Error::Stale);
        }
        let target = lookup(state, self.caller, &self.target.installed).ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&target, &self.target) {
            return Err(Error::Stale);
        }
        // A newer reserve requires that operation's actual owed SourceUse or
        // typed repair proof. Historical context alone never bypasses its drain.
        if let Some(cell) = state.reservations.get(&actor_key(self.caller)) {
            let held = lock(cell)?;
            if !matches!(&held.phase, seal::Phase::Terminal)
                && !(held.phase.owes_origin()
                    && same_source_scope(held.origin.source(), &self.source)
                    && held.phase.allows_member(state, self.caller, self.member)
                    && !held.quiescent.contains_key(self.member))
            {
                return Err(Error::Busy);
            }
        }
        let actor = state
            .actors
            .get(&actor_key(self.caller))
            .ok_or(Error::Stale)?;
        let issued = actor.issued.get(self.member).ok_or(Error::Stale)?;
        let member = state
            .members
            .get(&member_key(self.caller, self.member)?)
            .ok_or(Error::Stale)?;
        if actor.process != *self.process
            || member.process != *self.process
            || member.unknown
            || !issued.connection.same(self.connection)
            || !member.connection.same(self.connection)
            || issued
                .target
                .as_ref()
                .is_none_or(|target| !target.ptr_eq(&Arc::downgrade(&self.target.installed)))
            || !Arc::ptr_eq(&member.source, &self.source)
            || member.registration != self.registration
            || !same_source_scope(&self.source, &self.target.installed.source)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        self.target
            .current()
            .map_err(|_| "normalized target resources changed")?;
        if Process::observe(self.caller.pid).as_ref() != Some(self.process) {
            return Err("normalized target caller changed".into());
        }
        let state = lock(self.state).map_err(|_| "normalized target busy")?;
        self.matches(&state)
            .map_err(|_| "normalized target member changed".into())
    }
}

fn selected(
    state: &State,
    peer: &Peer,
    connection: &Connection,
    request: &NormalizedMemberRequest<'_>,
) -> Result<(Arc<NormalizedTarget>, Arc<slot::SourceHandle>, String), Error> {
    selected_owned(state, &peer.caller, &peer.process, connection, request)
}
pub(in crate::harness::codex_source_registry) fn selected_owned(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    connection: &Connection,
    request: &NormalizedMemberRequest<'_>,
) -> Result<(Arc<NormalizedTarget>, Arc<slot::SourceHandle>, String), Error> {
    if !hex(request.assignment, 32) || !hex(request.context, 64) || !hex(request.operation, 32) {
        return Err(Error::Denied);
    }
    let target = state
        .normalized_targets
        .get(&open_key(caller, request.assignment))
        .cloned()
        .ok_or(Error::Stale)?;
    if !target.active.load(Ordering::Acquire) || target.installed.context != request.context {
        return Err(Error::Stale);
    }
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    let issued = actor.issued.get(request.member).ok_or(Error::Stale)?;
    if issued.operation == request.operation {
        return Err(Error::Denied);
    }
    let member = state
        .members
        .get(&member_key(caller, request.member)?)
        .ok_or(Error::Stale)?;
    let record = state
        .target_opens
        .get(&open_key(caller, &issued.operation))
        .ok_or(Error::Stale)?;
    if record.caller != *caller || record.process != *process || record.context != request.context {
        return Err(Error::Stale);
    }
    record.admits_preload(request.member, request.operation)?;
    if state
        .preloads
        .contains_key(&member_key(caller, request.member)?)
        && !record.preloads.contains(request.operation)
    {
        return Err(Error::Stale);
    }
    if actor.process != *process
        || member.process != *process
        || member.unknown
        || !connection.is_live()
        || !issued.connection.same(connection)
        || !member.connection.same(connection)
        || issued
            .target
            .as_ref()
            .is_none_or(|issued| !issued.ptr_eq(&Arc::downgrade(&target.installed)))
        || !same_source_scope(&member.source, &target.installed.source)
    {
        return Err(Error::Stale);
    }
    Ok((target, member.source.clone(), member.registration.clone()))
}

impl Registry {
    pub fn preload_normalized_target(
        &self,
        env: &Env,
        peer: &Peer,
        connection: &Connection,
        request: NormalizedMemberRequest<'_>,
    ) -> Result<Preload, Error> {
        if !peer.current() || !connection.is_live() {
            return Err(Error::Denied);
        }
        let (target, source, registration) = {
            let state = lock(&self.state)?;
            selected(&state, peer, connection, &request)?
        };
        {
            let state = lock(&self.state)?;
            let key = member_key(&peer.caller, request.member)?;
            let current = state.members.get(&key).ok_or(Error::Stale)?;
            if current.operation == request.operation
                && current.readiness.role.is_some()
                && !state.preloads.contains_key(&key)
            {
                return Ok(Preload {
                    member: request.member.into(),
                    operation: request.operation.into(),
                    registration: current.registration.clone(),
                    source: current.source.clone(),
                });
            }
        }
        let worker = preload_custody::enter_checked(
            self,
            peer,
            request.member,
            request.operation,
            connection,
            |state| {
                let (current, observed, actual_registration) =
                    selected(state, peer, connection, &request)?;
                if !Arc::ptr_eq(&current, &target)
                    || !Arc::ptr_eq(&observed, &source)
                    || actual_registration != registration
                {
                    return Err(Error::Stale);
                }
                if let Some(cell) = state.reservations.get(&actor_key(&peer.caller))
                    && !matches!(&lock(cell)?.phase, seal::Phase::Terminal)
                {
                    return Err(Error::Busy);
                }
                let open = state
                    .actors
                    .get(&actor_key(&peer.caller))
                    .and_then(|actor| actor.issued.get(request.member))
                    .ok_or(Error::Stale)?
                    .operation
                    .clone();
                let record = state
                    .target_opens
                    .get_mut(&open_key(&peer.caller, &open))
                    .ok_or(Error::Stale)?;
                record.admits_preload(request.member, request.operation)?;
                record.preloads.insert(request.operation.into());
                record.latest_preload = Some(request.operation.into());
                Ok(())
            },
        )?;
        let permission = NormalizedRead {
            state: &self.state,
            target,
            caller: &peer.caller,
            process: &peer.process,
            connection,
            member: request.member,
            source,
            registration,
            scope: worker.scope(),
        };
        let source = match slot::preload_normalized(env, &permission, request.operation)
            .map_err(|_| Error::Unavailable)?
        {
            slot::SourceProgress::Ready(source) => Arc::new(source),
            slot::SourceProgress::Waiting => return Err(Error::Busy),
        };
        permission.current().map_err(|_| Error::Stale)?;
        let reader_owned = {
            let state = lock(&self.state)?;
            permission.matches(&state)?;
            state
                .members
                .get(&member_key(&peer.caller, request.member)?)
                .ok_or(Error::Stale)?
                .readiness
                .reader_owned
        };
        let readiness =
            SourceReadiness::target(permission.target.installed.resource.clone(), reader_owned)?;
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let mut state = lock(&self.state)?;
        permission.matches(&state)?;
        let key = member_key(&peer.caller, request.member)?;
        let old = state.members.get(&key).ok_or(Error::Stale)?;
        if !same_source_scope(&old.source, &source)
            || !revision_descends(
                source.credential_generation(),
                source.digest(),
                old.source.credential_generation(),
                old.source.digest(),
            )
        {
            return Err(Error::Stale);
        }
        if old.operation == request.operation {
            // Replaying an accepted preload may not silently replace its C or
            // registration. The next native operation can observe a later C.
            if old.source.credential_generation() != source.credential_generation()
                || old.source.digest() != source.digest()
            {
                return Err(Error::Stale);
            }
            return Ok(Preload {
                member: request.member.into(),
                operation: request.operation.into(),
                registration: old.registration.clone(),
                source: old.source.clone(),
            });
        }
        let version = version(&mut state)?;
        state.members.insert(
            key,
            Member {
                caller: peer.caller.clone(),
                process: peer.process.clone(),
                operation: request.operation.into(),
                registration: registration.clone(),
                connection: connection.clone(),
                version,
                source: source.clone(),
                readiness,
                unknown: false,
                work_rebound: false,
            },
        );
        Ok(Preload {
            member: request.member.into(),
            operation: request.operation.into(),
            registration,
            source,
        })
    }

    pub fn ready_normalized_target(
        &self,
        env: &Env,
        peer: &Peer,
        connection: &Connection,
        assignment: &str,
        context: &str,
        report: TargetReady<'_>,
    ) -> Result<(), Error> {
        if !peer.current() || !connection.is_live() {
            return Err(Error::Denied);
        }
        let request = NormalizedMemberRequest {
            assignment,
            context,
            member: report.member,
            operation: report.operation,
        };
        let (target, source, registration) = {
            let state = lock(&self.state)?;
            selected(&state, peer, connection, &request)?
        };
        if registration != report.registration
            || source.assignment_generation() != report.assignment_generation
        {
            return Err(Error::Stale);
        }
        let desired = reported_role(&report.ready, &source)?;
        {
            let state = lock(&self.state)?;
            let member = state
                .members
                .get(&member_key(&peer.caller, report.member)?)
                .ok_or(Error::Stale)?;
            if member.operation != report.operation {
                return Err(Error::Stale);
            }
            if member.readiness.role == Some(desired) {
                return Ok(());
            }
        }
        let worker = preload_custody::enter_completion(
            self,
            peer,
            report.member,
            report.operation,
            connection,
        )?;
        let permission = NormalizedRead {
            state: &self.state,
            target,
            caller: &peer.caller,
            process: &peer.process,
            connection,
            member: report.member,
            source,
            registration,
            scope: worker.scope(),
        };
        permission.current().map_err(|_| Error::Stale)?;
        // A negative policy report releases this exact preload but never mints
        // Loaded/Ready, so it requires no new credential owner permission.
        if desired != ReadyRole::PolicyUnavailable {
            slot::validate_normalized(env, &permission).map_err(|_| Error::Unavailable)?;
        }
        let retired = {
            let mut state = lock(&self.state)?;
            permission.matches(&state)?;
            let key = member_key(&peer.caller, report.member)?;
            let member = state.members.get_mut(&key).ok_or(Error::Stale)?;
            if member.operation != report.operation {
                return Err(Error::Stale);
            }
            member.readiness.complete_recorded(desired)?;
            state.preloads.remove(&key)
        };
        drop(retired);
        Ok(())
    }
}

pub(in crate::harness::codex_source_registry) fn note_owned_preload(
    state: &mut State,
    caller: &slot::Caller,
    member: &str,
    operation: &str,
) -> Result<(), Error> {
    let open = state
        .actors
        .get(&actor_key(caller))
        .and_then(|actor| actor.issued.get(member))
        .ok_or(Error::Stale)?
        .operation
        .clone();
    let record = state
        .target_opens
        .get_mut(&open_key(caller, &open))
        .ok_or(Error::Stale)?;
    record.admits_preload(member, operation)?;
    record.preloads.insert(operation.into());
    record.latest_preload = Some(operation.into());
    Ok(())
}
