// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-only PRELOAD retirement. Correlation metadata has no conversion to
//! a Peer, source permission, target role, or new work Scope.
use super::*;

pub struct MemberRetirementRequest<'a> {
    pub operation: &'a str,
    pub member: &'a str,
    pub preload_operation: &'a str,
    pub preload_sequence: PreloadSequence,
    pub registration: Option<&'a str>,
}
pub enum MemberRetirementPreparation {
    Pending(Box<OwnedMemberRetirement>),
    Closed,
}
pub struct OwnedMemberRetirement {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    member: String,
    body: preload_sequence::Retirement,
    _turn: Arc<crate::harness::account_work::ReleaseTurn>,
}
impl std::fmt::Debug for OwnedMemberRetirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedMemberRetirement(redacted)")
    }
}

fn open_for_member<'a>(state: &'a State, caller: &slot::Caller, member: &str) -> Option<&'a str> {
    state
        .actors
        .get(&actor_key(caller))?
        .issued
        .get(member)
        .map(|issued| issued.operation.as_str())
}

impl Registry {
    /// The authenticated front door calls this before taking any queued GUI
    /// PRELOAD/READY captures. A recorded fence precedes releasing those
    /// captures; an unaccepted/unknown request is never reported Closed.
    pub fn prepare_member_retirement(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: MemberRetirementRequest<'_>,
    ) -> Result<MemberRetirementPreparation, Error> {
        let key = member_key(caller, request.member)?;
        if !hex(request.operation, 32)
            || !hex(request.preload_operation, 32)
            || request
                .registration
                .is_some_and(|registration| !hex(registration, 32))
            || !connection.is_live()
        {
            return Err(Error::Denied);
        }
        let body = preload_sequence::Retirement {
            operation: request.operation.into(),
            sequence: request.preload_sequence,
            preload: request.preload_operation.into(),
            registration: request.registration.map(str::to_owned),
        };
        let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let app = {
            let state = lock(&self.state)?;
            if state
                .preload_sequences
                .get(&key)
                .is_some_and(|ledger| ledger.exact_closed(&process, &body))
            {
                return Ok(MemberRetirementPreparation::Closed);
            }
            state.app_lifetime.clone().ok_or(Error::Denied)?
        };
        // A real release turn counts the mutation without reopening ordinary
        // admission. After Drained is sealed only exact Closed replay above
        // is possible; no late highwater/fence mutation is permitted.
        let turn = Arc::new(app.release_turn().ok_or(Error::Unavailable)?);
        let (pending, retired_resources, release) = {
            let mut state = lock(&self.state)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            if actor.process != process || !actor.issued.contains_key(request.member) {
                return Err(Error::Stale);
            }
            let mut ledger = if let Some(ledger) = state.preload_sequences.get(&key) {
                ledger.clone()
            } else {
                // No sequence exists only when the delayed PRELOAD was never
                // accepted. Actual membership is still mandatory, and there
                // can be no uncorrelated legacy credential occurrence.
                if request.registration.is_some()
                    || state.members.contains_key(&key)
                    || state.preloads.contains_key(&key)
                    || state.unrelated.contains_key(&key)
                {
                    return Err(Error::Stale);
                }
                preload_sequence::capacity(&state, &key)?;
                preload_sequence::Ledger::new(
                    process.clone(),
                    preload_sequence::Attempt {
                        sequence: request.preload_sequence,
                        operation: request.preload_operation.into(),
                        origin: preload_sequence::Origin::RetirementOnly,
                    },
                )
            };
            if ledger.exact_closed(&process, &body) {
                return Ok(MemberRetirementPreparation::Closed);
            }
            let already_fenced = ledger.exact_fence(&process, &body);
            ledger.fence(&process, body.clone())?;
            let release = ledger.retain_release(&turn);
            resources::check_member_update(&state, caller, request.member)?;
            let open = open_for_member(&state, caller, request.member)
                .ok_or(Error::Stale)?
                .to_owned();
            if let Some(record) = state.target_opens.get(&open_key(caller, &open))
                && (record.caller != *caller
                    || record.process != process
                    || record.member != request.member
                    || record.cleanup.is_some()
                    || record.closed.is_some()
                    || record
                        .member_retirement
                        .as_ref()
                        .is_some_and(|old| old != &body))
            {
                return Err(Error::Stale);
            }
            let pending = state.preloads.get(&key).cloned();
            if !already_fenced && let Some(cell) = state.reservations.get(&actor_key(caller)) {
                let held = lock(cell)?;
                for entry in held
                    .target_constructors
                    .values()
                    .filter(|entry| entry.work.member == request.member)
                {
                    ledger.retain_retirement(&entry.work.scope)?;
                }
            }
            if let Some(record) = state.target_opens.get_mut(&open_key(caller, &open)) {
                record.member_retirement = Some(body.clone());
            }
            ledger.cancel_captured_work();
            state.preload_sequences.insert(key.clone(), ledger);
            if let Some(member) = state.members.get_mut(&key) {
                member.unknown = true;
            }
            let resources = resources::revoke_member(&mut state, caller, request.member);
            (pending, resources, release)
        };
        // Cancellation may release the stored original Scope. Never drop a
        // credential/kernel/Scope capture while State is held.
        if let Some(pending) = pending {
            pending.revoke_departed();
        }
        drop(retired_resources);
        Ok(MemberRetirementPreparation::Pending(Box::new(
            OwnedMemberRetirement {
                state: Arc::downgrade(&self.state),
                caller: caller.clone(),
                process,
                connection: connection.clone(),
                member: request.member.into(),
                body,
                _turn: release,
            },
        )))
    }

    /// True means the exact accepted retirement is Closed. False preserves
    /// the recorded fence while actual native workers/uses/captures retire.
    pub fn retire_member_owned(&self, command: Box<OwnedMemberRetirement>) -> Result<bool, Error> {
        if command
            .state
            .upgrade()
            .is_none_or(|state| !Arc::ptr_eq(&state, &self.state))
            || !command.connection.is_live()
            || Process::observe(command.caller.pid).as_ref() != Some(&command.process)
        {
            return Err(Error::Stale);
        }
        let key = member_key(&command.caller, &command.member)?;
        let retired = {
            let mut state = lock(&self.state)?;
            let ledger = state.preload_sequences.get(&key).ok_or(Error::Stale)?;
            if ledger.exact_closed(&command.process, &command.body) {
                return Ok(true);
            }
            if !ledger.exact_fence(&command.process, &command.body) {
                return Err(Error::Stale);
            }
            if reservation::has_credential_uses(&state, &key)
                || preload_custody::running(&state, &key)
            {
                return Ok(false);
            }
            resources::check_retirement(&state, &command.caller, &command.member)?;
            resources::check_retired_member_captures(&state, &command.caller, &command.member)?;
            let constructor = retire_constructor(&state, &command.caller, &command.member)?;
            let pending = state.preloads.remove(&key);
            let member = state.members.remove(&key);
            // Issued remains until every actual witness retires, so census
            // continues to see an unresolved member throughout this interval.
            (constructor, pending, member)
        };
        drop(retired);
        let mut state = lock(&self.state)?;
        if !state.preload_sequences.get(&key).is_some_and(|ledger| {
            ledger.exact_fence(&command.process, &command.body) && ledger.retired()
        }) || !resources::member_captures_retired(&state, &command.caller, &command.member)
            || reservation::has_credential_uses(&state, &key)
        {
            return Ok(false);
        }
        let open = open_for_member(&state, &command.caller, &command.member)
            .ok_or(Error::Stale)?
            .to_owned();
        if let Some(record) = state
            .target_opens
            .get_mut(&open_key(&command.caller, &open))
        {
            if record.member_retirement.as_ref() != Some(&command.body) {
                return Err(Error::Stale);
            }
            record.member_retired = true;
        }
        let ledger = state.preload_sequences.get_mut(&key).ok_or(Error::Stale)?;
        ledger.close(&command.process, &command.body)?;
        let release = ledger.take_release();
        state
            .actors
            .get_mut(&actor_key(&command.caller))
            .ok_or(Error::Stale)?
            .issued
            .remove(&command.member);
        let normalized = normalize::take_unused(&mut state);
        drop(state);
        drop(normalized);
        drop(release);
        Ok(true)
    }
}
