// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Existing OPEN correlation only. Rebinding never allocates a member, restores
//! Ready, or obtains a source from the current account selector.
use super::*;

pub enum WorkRebindKind<'a> {
    Ordinary,
    Derived {
        use_member: &'a str,
        use_operation: &'a str,
        capability: &'a str,
    },
    Target {
        assignment: &'a str,
        context: &'a str,
    },
}
pub struct WorkRebindRequest<'a> {
    pub operation: &'a str,
    pub member: &'a str,
    pub kind: WorkRebindKind<'a>,
}
impl std::fmt::Debug for WorkRebindRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkRebindRequest(redacted)")
    }
}

fn original_body(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    request: &WorkRebindRequest<'_>,
    issued: &Issued,
) -> Result<(), Error> {
    if issued.operation != request.operation {
        return Err(Error::Stale);
    }
    match &request.kind {
        WorkRebindKind::Ordinary => {
            if issued.parent_use.is_some() || issued.target.is_some() {
                return Err(Error::Stale);
            }
        }
        WorkRebindKind::Derived {
            use_member,
            use_operation,
            capability,
        } => {
            let nonce = UseNonce::parse(use_operation).ok_or(Error::Denied)?;
            if !hex(use_member, 32)
                || !hex(capability, 64)
                || issued.parent_use.as_ref()
                    != Some(&ParentUse {
                        member: (*use_member).into(),
                        nonce,
                        capability: (*capability).into(),
                    })
            {
                return Err(Error::Stale);
            }
        }
        WorkRebindKind::Target {
            assignment,
            context,
        } => {
            if !hex(assignment, 32)
                || !hex(context, 64)
                || issued.parent_use.is_some()
                || issued.target.is_none()
            {
                return Err(Error::Stale);
            }
            let record = state
                .target_opens
                .get(&open_key(caller, request.operation))
                .ok_or(Error::Stale)?;
            if record.assignment != *assignment || record.context != *context {
                return Err(Error::Stale);
            }
        }
    }
    if issued.target.is_some() {
        let record = state
            .target_opens
            .get(&open_key(caller, request.operation))
            .ok_or(Error::Stale)?;
        if record.caller != *caller
            || record.process != *process
            || record.member != request.member
            || record.parent_use != issued.parent_use
            || record.cleanup.is_some()
            || record.closed.is_some()
            || record.member_retirement.is_some()
            || record.member_retired
        {
            return Err(Error::Stale);
        }
    }
    Ok(())
}

impl Registry {
    /// Authenticate an existing allocation on a replacement work connection.
    /// The front door supplies the actual socket caller; no foreground claim
    /// or generic Peer/new membership authority is created here.
    pub fn rebind_member_work(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: WorkRebindRequest<'_>,
    ) -> Result<(), Error> {
        if !hex(request.operation, 32) || !connection.is_live() {
            return Err(Error::Denied);
        }
        let key = member_key(caller, request.member)?;
        let observed = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if observed.birth != caller.birth {
            return Err(Error::Stale);
        }
        let (retired_pending, turn) = {
            let mut state = lock(&self.state)?;
            if !connection.is_live() || Process::observe(caller.pid).as_ref() != Some(&observed) {
                return Err(Error::Stale);
            }
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            if actor.process != observed {
                return Err(Error::Stale);
            }
            let issued = actor.issued.get(request.member).ok_or(Error::Stale)?;
            original_body(&state, caller, &observed, &request, issued)?;
            if issued.connection.same(connection) {
                // Exact correlation without mapping/resource mutation is safe
                // after App sealing. It grants no Ready or activity permission.
                return Ok(());
            }
            if issued.connection.is_live() {
                return Err(Error::Busy);
            }
            let original = issued.connection.clone();
            reservation_allows_member(&state, caller, request.member)?;
            resources::check_retirement(&state, caller, request.member)?;
            let cell = state.reservations.get(&actor_key(caller)).cloned();
            let mut held = cell.as_ref().map(|cell| lock(cell)).transpose()?;
            if let Some(held) = held.as_ref() {
                for entry in held
                    .target_constructors
                    .values()
                    .filter(|entry| entry.work.member == request.member)
                {
                    // In particular an unstarted queued READY is still a real
                    // capture, even before its worker flag is set.
                    if !entry.connection.same(&original)
                        || entry.work.worker.load(Ordering::Acquire)
                        || Arc::strong_count(&entry.work) != 1
                    {
                        return Err(Error::Busy);
                    }
                }
            }
            let pending = state.preloads.get(&key).cloned();
            if pending
                .as_ref()
                .is_some_and(|pending| !pending.connection.same(&original))
            {
                return Err(Error::Stale);
            }
            let replacement = pending
                .as_ref()
                .map(|pending| pending.prepare_rebound(connection))
                .transpose()?;
            let turn = state
                .app_lifetime
                .as_ref()
                .and_then(|app| app.release_turn())
                .ok_or(Error::Busy)?;
            // All fallible validation precedes mutation. No filesystem/kernel
            // probe, resource drop or source operation runs under State.
            if let Some(held) = held.as_mut() {
                for entry in held
                    .target_constructors
                    .values_mut()
                    .filter(|entry| entry.work.member == request.member)
                {
                    entry.connection = connection.clone();
                }
            }
            let actor = state
                .actors
                .get_mut(&actor_key(caller))
                .expect("validated actor");
            actor
                .issued
                .get_mut(request.member)
                .expect("validated OPEN")
                .connection = connection.clone();
            if let Some(member) = state.members.get_mut(&key) {
                member.connection = connection.clone();
                // A bound connection is not a new source/readiness admission.
                member.unknown = true;
                member.work_rebound = true;
            }
            if let Some(pending) = &pending {
                pending.revoked.store(true, Ordering::Release);
            }
            if let Some(replacement) = replacement {
                state.preloads.insert(key, replacement);
            }
            drop(held);
            (pending, turn)
        };
        if let Some(pending) = retired_pending {
            pending.release_rebound();
        }
        drop(turn);
        Ok(())
    }
}
