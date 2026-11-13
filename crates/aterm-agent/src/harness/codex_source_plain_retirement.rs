// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-only cleanup for authoritative empty/unrelated members. No Peer,
//! source, Ready, or original work-connection authority is minted here.
use super::*;
fn retired(actor: &Actor, member: &str) -> bool {
    member
        .strip_prefix(&actor.epoch)
        .and_then(|ordinal| u64::from_str_radix(ordinal, 16).ok())
        .is_some_and(|ordinal| ordinal > 0 && ordinal <= actor.highwater)
}
impl Registry {
    pub fn retire_nonpreloaded_member(
        &self,
        caller: &slot::Caller,
        cleanup: &Connection,
        member: &str,
        registration: Option<&str>,
    ) -> Result<(), Error> {
        let key = member_key(caller, member)?;
        if !cleanup.is_live() || registration.is_some_and(|value| !hex(value, 32)) {
            return Err(Error::Denied);
        }
        let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let app = {
            let state = lock(&self.state)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            if actor.process != process {
                return Err(Error::Stale);
            }
            // The owned managed route is never replaced by this cleanup verb.
            if state.plain_retiring.contains_key(&key) {
                return Err(Error::Busy);
            }
            if state.preload_sequences.contains_key(&key) {
                return Err(Error::Denied);
            }
            if !actor.issued.contains_key(member) {
                // Definitive prior native retirement: a negative observation,
                // not acceptance of a new cleanup body or a wildcard R grant.
                return if retired(actor, member) {
                    Ok(())
                } else {
                    Err(Error::Stale)
                };
            }
            state.app_lifetime.clone().ok_or(Error::Denied)?
        };
        let turn = app.release_turn().ok_or(Error::Unavailable)?;
        let (normalized, constructor, resources, launches) = {
            let mut state = lock(&self.state)?;
            if !cleanup.is_live() {
                return Err(Error::Stale);
            }
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            if actor.process != process {
                return Err(Error::Stale);
            }
            if state.plain_retiring.contains_key(&key) {
                return Err(Error::Busy);
            }
            if state.preload_sequences.contains_key(&key) {
                return Err(Error::Denied);
            }
            let Some(allocation) = actor.issued.get(member) else {
                return if retired(actor, member) {
                    Ok(())
                } else {
                    Err(Error::Stale)
                };
            };
            if reservation::has_credential_uses(&state, &key)
                || preload_custody::running(&state, &key)
            {
                return Err(Error::Busy);
            }
            if state.preload_sequences.contains_key(&key)
                || state.preloads.contains_key(&key)
                || state.members.contains_key(&key)
                || allocation.target.is_some()
            {
                return Err(Error::Denied);
            }
            match state.unrelated.get(&key) {
                Some(current)
                    if current.process == process
                        && current.operation == allocation.operation
                        && current.connection.same(&allocation.connection)
                        && registration == Some(current.registration.as_str()) => {}
                Some(_) => return Err(Error::Stale),
                None if registration.is_none() => {}
                None => return Err(Error::Stale),
            }
            reservation::target::resources::check_retirement(&state, caller, member)?;
            // Reserve the fallible version bump before any retirement mutation.
            version(&mut state)?;
            let constructor = reservation::target::retire_constructor(&state, caller, member)?;
            let resources =
                reservation::target::resources::revoke_member(&mut state, caller, member);
            // Keep repeated cleanup Waiting until every removed release object
            // and this App release turn have actually dropped outside State.
            state.plain_retiring.insert(key.clone(), ());
            state.unrelated.remove(&key);
            let actor = state
                .actors
                .get_mut(&actor_key(caller))
                .ok_or(Error::Stale)?;
            actor.issued.remove(member);
            let empty = actor.issued.is_empty();
            // Terminating the final never-loaded constructor also releases its
            // bootstrap exclusion. Other issued consumers keep their own gate.
            let launches = if empty {
                state
                    .launches
                    .values_mut()
                    .filter(|launch| {
                        launch.sid == caller.sid && launch.process.as_ref() == Some(&process)
                    })
                    .map(|launch| {
                        if let Some(initial) = &launch.initial {
                            initial.alive.store(false, Ordering::Release);
                        }
                        // Retain the qualified launch permission for subsequent
                        // factories; release only the now-unused constructor gate.
                        (launch.gate.take(), launch._scope.take())
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let normalized = reservation::target::normalize::take_unused(&mut state);
            (normalized, constructor, resources, launches)
        };
        // Real release objects and constructor gates drop outside State, before
        // the successful Closed reply. EOF alone never executes this path.
        drop((normalized, constructor, resources, launches));
        drop(turn);
        // Every release/account guard is gone. Completion must not lose the
        // sole marker owner merely because an unrelated State reader is busy.
        self.state
            .lock()
            .map_err(|_| Error::Unavailable)?
            .plain_retiring
            .remove(&key);
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "codex_source_plain_retirement_tests.rs"]
mod tests;
