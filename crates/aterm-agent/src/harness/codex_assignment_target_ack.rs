// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The immutable ACK observation is signed with the canonical Book. Reading an
//! Applied phase is not a constructor for the live normalization receipt.
use super::*;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::harness) struct Observation {
    pub operation: String,
    pub context: String,
    pub resource_operation: String,
    pub resource_receipt: String,
    pub member: String,
    pub registration: String,
    pub boundary: String,
    pub assignment_generation: u64,
    pub credential_generation: u64,
    pub digest: String,
    pub account: String,
    pub user: String,
    pub auth_generation: u64,
    pub owner_generation: u64,
    pub source_generation: u64,
    pub login_generation: u64,
}
impl Observation {
    pub(super) fn valid(&self) -> bool {
        [
            &self.operation,
            &self.resource_operation,
            &self.member,
            &self.registration,
            &self.boundary,
        ]
        .iter()
        .all(|value| hex(value, 32))
            && [
                &self.context,
                &self.resource_receipt,
                &self.digest,
                &self.account,
                &self.user,
            ]
            .iter()
            .all(|value| hex(value, 64))
            && self.assignment_generation > 0
            && self.credential_generation > 0
    }
}

/// Only an actual typed resource permit joined to a durable Book write creates
/// this. The fields are deliberately not serde and the receipt is not Clone.
pub(in crate::harness) struct AppliedReceipt {
    pub(super) caller: Caller,
    pub(super) binding: owner::GrantBinding,
    pub(super) observation: Observation,
    pub(super) lease: Lease,
}
impl AppliedReceipt {
    pub(in crate::harness) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(in crate::harness) fn observation(&self) -> &Observation {
        &self.observation
    }
    pub(in crate::harness) fn matches_source(
        &self,
        source: &crate::harness::codex_session_slot::SourceHandle,
    ) -> bool {
        source.binding() == &self.binding
            && source.assignment_generation() == self.observation.assignment_generation
            && source.native_intent() == Some(self.observation.operation.as_str())
            && source.account() == self.observation.account
            && source.user() == self.observation.user
            && source.credential_generation() >= self.observation.credential_generation
            && (source.credential_generation() != self.observation.credential_generation
                || source.digest() == self.observation.digest)
    }
    pub(in crate::harness) fn with_current<R>(
        &self,
        commit: &AssignmentCommit,
        inspect: impl FnOnce(&BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        if !self.matches(commit) {
            return Err("Applied receipt assignment changed".into());
        }
        self.with_current_book(inspect)
    }
    /// Authenticate an already issued receipt for cleanup correlation only.
    /// This does not reconstruct installation or target read authority.
    pub(in crate::harness) fn with_current_book<R>(
        &self,
        inspect: impl FnOnce(&BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        let claimed = self
            .lease
            .try_lock()
            .map_err(|_| "assignment custody busy")?;
        if claimed.pending_install.is_some()
            || !claimed.book.valid(&self.caller)
            || claimed.book.current.phase != Phase::Applied
            || claimed.book.current.binding != self.binding
            || claimed.book.current.generation != self.observation.assignment_generation
            || claimed.book.current.authority
                != (SourceAuthority::Assignment {
                    operation: self.observation.operation.clone(),
                })
            || claimed.book.current.boundary.as_deref() != Some(self.observation.boundary.as_str())
            || !claimed
                .book
                .acknowledged
                .iter()
                .any(|prior| prior == &self.observation)
            || claimed.book.consumer != Consumer::observe(&self.caller)?
        {
            return Err("Applied receipt is not the current assignment".into());
        }
        claimed.store.unchanged().map_err(str::to_owned)?;
        let namespace = claimed.store.physical_identity().map_err(str::to_owned)?;
        let result = inspect(&namespace)?;
        claimed.store.unchanged().map_err(str::to_owned)?;
        Ok(result)
    }
    pub(in crate::harness) fn matches(&self, commit: &AssignmentCommit) -> bool {
        self.caller == commit.caller
            && self.binding == commit.binding
            && self.observation.operation == commit.operation
            && self.observation.boundary == commit.boundary
            && self.observation.assignment_generation == commit.generation
            && Arc::ptr_eq(&self.lease, &commit._lease)
    }
}

pub(super) fn records_valid(book: &Book) -> bool {
    let records = &book.acknowledged;
    records.len() <= MAX_LIVE
        && records.iter().all(|ack| {
            ack.valid()
                && ack.assignment_generation <= book.clock
                && book
                    .prepared
                    .iter()
                    .any(|prepared| prepared.matches_ack(ack))
        })
        && records.iter().enumerate().all(|(index, value)| {
            records[..index]
                .iter()
                .all(|prior| prior.operation != value.operation)
        })
}

pub(in crate::harness) fn acknowledge(
    env: &Env,
    permit: &super::super::codex_source_registry::TargetAckPermit,
) -> Result<AppliedReceipt, String> {
    let assignment = permit.assignment();
    let observation = permit.observation();
    if !observation.valid() {
        return Err("target ACK observation invalid".into());
    }
    let home = env.home.as_deref().ok_or("assignment home unavailable")?;
    if !permit.matches_home(home) {
        return Err("target ACK namespace changed".into());
    }
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let lease = assignment._lease.clone();
    let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
    prepare::install::recover(&mut claimed, &identity)?;
    if let Some(pending) = &claimed.pending_install {
        let candidate = pending.book();
        if candidate.current.phase != Phase::Applied
            || candidate.current.binding != *assignment.binding()
            || candidate.current.generation != assignment.generation()
            || candidate.current.authority
                != (SourceAuthority::Assignment {
                    operation: assignment.operation().into(),
                })
            || candidate.current.boundary.as_deref() != Some(observation.boundary.as_str())
            || !candidate
                .acknowledged
                .iter()
                .any(|prior| prior == observation)
        {
            return Err("target ACK has another pending publication".into());
        }
        permit.settle_started()?;
        let exact = candidate.clone();
        prepare::install::write(&mut claimed, &identity, exact)?;
    }
    let current = &claimed.book.current;
    if !claimed.book.valid(assignment.caller())
        || current.binding != *assignment.binding()
        || current.generation != assignment.generation()
        || current.authority
            != (SourceAuthority::Assignment {
                operation: assignment.operation().into(),
            })
        || current.boundary.as_deref() != Some(observation.boundary.as_str())
    {
        return Err("target ACK assignment changed".into());
    }
    let prior = claimed
        .book
        .acknowledged
        .iter()
        .find(|prior| prior.operation == observation.operation);
    if let Some(prior) = prior {
        if prior != observation || current.phase != Phase::Applied {
            return Err("target ACK body changed".into());
        }
    } else {
        if current.phase != Phase::AwaitingAck || claimed.book.acknowledged.len() >= MAX_LIVE {
            return Err("target ACK unavailable".into());
        }
        // Account/owner custody precedes Book custody. Release the Book while
        // validating actual current policy and immutable Slot/owner authority.
        drop(claimed);
        super::super::codex_session_slot::validate_target_ack(env, permit)?;
        claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
        prepare::install::recover(&mut claimed, &identity)?;
        if claimed.book.consumer != Consumer::observe(assignment.caller())?
            || claimed.book.current.binding != *assignment.binding()
            || claimed.book.current.generation != assignment.generation()
            || claimed.book.current.phase != Phase::AwaitingAck
            || claimed.book.current.authority
                != (SourceAuthority::Assignment {
                    operation: assignment.operation().into(),
                })
            || claimed.book.current.boundary.as_deref() != Some(observation.boundary.as_str())
            || claimed
                .book
                .acknowledged
                .iter()
                .any(|prior| prior.operation == observation.operation)
        {
            return Err("target ACK publication raced".into());
        }
        permit.current()?;
        let mut next = claimed.book.clone();
        next.schema = 5;
        next.current.phase = Phase::Applied;
        next.acknowledged.push(observation.clone());
        permit.before_write()?;
        prepare::install::write(&mut claimed, &identity, next)?;
    }
    // Acknowledging phase fences explicit retirement throughout the write.
    // Cancellation after durability cannot erase or replace the observation.
    claimed.store.unchanged().map_err(str::to_owned)?;
    drop(claimed);
    Ok(AppliedReceipt {
        caller: assignment.caller().clone(),
        binding: assignment.binding().clone(),
        observation: observation.clone(),
        lease,
    })
}
