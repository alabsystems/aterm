// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Read-only projection of actual native use and accepted parent relationships.
use super::super::proof_observation::{SourceProofDerived, SourceProofUse};
use super::*;

struct Row {
    key: String,
    counter: u64,
    prefix: u64,
    role: ReadyRole,
    registration: String,
    capability: String,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    scope: crate::harness::account_work::Scope,
}
pub(in crate::harness::codex_source_registry) struct ProofUses {
    rows: Vec<Row>,
    derived: Vec<SourceProofDerived>,
}
fn derived(state: &State, caller: &slot::Caller) -> Result<Vec<SourceProofDerived>, Error> {
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    let mut result = Vec::new();
    for (child, issued) in &actor.issued {
        if let Some(parent) = &issued.parent_use {
            let key = member_key(caller, &parent.member)?;
            let uses = state.uses.get(&key).ok_or(Error::Stale)?;
            let active = uses.active.get(&parent.nonce.counter).ok_or(Error::Stale)?;
            if uses.prefix != parent.nonce.prefix || active.capability != parent.capability {
                return Err(Error::Stale);
            }
            result.push(SourceProofDerived {
                child: child.clone(),
                parent: parent.member.clone(),
                nonce: format!("{:016x}{:016x}", parent.nonce.prefix, parent.nonce.counter),
            });
        }
    }
    Ok(result)
}
impl ProofUses {
    pub(in crate::harness::codex_source_registry) fn counts(
        state: &State,
        caller: &slot::Caller,
    ) -> (usize, usize, usize) {
        let prefix = format!("{}:", actor_key(caller));
        state
            .uses
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .fold((0, 0, 0), |(active, pending, ending), (_, uses)| {
                (
                    active + uses.active.len(),
                    pending + uses.pending.len(),
                    ending + uses.ending.len(),
                )
            })
    }

    pub(in crate::harness::codex_source_registry) fn capture(
        state: &State,
        caller: &slot::Caller,
    ) -> Result<Self, Error> {
        let prefix = format!("{}:", actor_key(caller));
        let mut rows = Vec::new();
        for (key, uses) in state
            .uses
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
        {
            for (counter, active) in &uses.active {
                rows.push(Row {
                    key: key.clone(),
                    counter: *counter,
                    prefix: uses.prefix,
                    role: active.role,
                    registration: active.registration.clone(),
                    capability: active.capability.clone(),
                    source: Arc::clone(&active.source),
                    gate: active.gate.clone(),
                    scope: active._scope.clone(),
                });
            }
        }
        Ok(Self {
            rows,
            derived: derived(state, caller)?,
        })
    }
    pub(in crate::harness::codex_source_registry) fn observed(&self) -> bool {
        self.rows
            .iter()
            .all(|row| !row.scope.cancelled() && row.gate.current())
    }
    pub(in crate::harness::codex_source_registry) fn matches(
        &self,
        state: &State,
        caller: &slot::Caller,
    ) -> bool {
        self.rows.iter().all(|row| {
            state.uses.get(&row.key).is_some_and(|uses| {
                uses.prefix == row.prefix
                    && uses.active.get(&row.counter).is_some_and(|active| {
                        active.role == row.role
                            && active.registration == row.registration
                            && active.capability == row.capability
                            && Arc::ptr_eq(&active.source, &row.source)
                            && active.gate == row.gate
                            && !active._scope.cancelled()
                            && state
                                .app_lifetime
                                .as_ref()
                                .is_some_and(|app| active._scope.cancellation().owned_by(app))
                            && state.members.get(&row.key).is_some_and(|member| {
                                let name = row.key.rsplit(':').next().unwrap_or_default();
                                !member.unknown
                                    && member.connection.is_live()
                                    && &member.caller == caller
                                    && same_source_scope(&active.source, &member.source)
                                    && active.gate == member.readiness.identity
                                    && state.actors.get(&actor_key(caller)).is_some_and(|actor| {
                                        member.process == actor.process
                                            && actor.issued.get(name).is_some_and(|issued| {
                                                issued.connection.same(&member.connection)
                                            })
                                    })
                            })
                    })
            })
        }) && derived(state, caller).is_ok_and(|current| current == self.derived)
    }
    pub(in crate::harness::codex_source_registry) fn metadata(
        &self,
    ) -> (Vec<SourceProofUse>, Vec<SourceProofDerived>) {
        (
            self.rows
                .iter()
                .map(|row| SourceProofUse {
                    member: row.key.rsplit(':').next().unwrap_or_default().to_owned(),
                    nonce: format!("{:016x}{:016x}", row.prefix, row.counter),
                    manager: matches!(row.role, ReadyRole::Manager(_)),
                    reader: row.role == ReadyRole::ReaderLoaded,
                })
                .collect(),
            self.derived.clone(),
        )
    }
}
