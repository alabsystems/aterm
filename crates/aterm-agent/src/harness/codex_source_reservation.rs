// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Registry-owned activity and reservation custody. Handle drop and socket EOF
//! cannot attest that an SDK activity, reader, or derived factory has retired.

use super::*;
use crate::harness::codex_source_gate::SourceGateIdentity;

#[path = "codex_source_reservation_origin.rs"]
mod origin;
#[path = "codex_source_prepared_policy.rs"]
mod prepared_policy;
#[path = "codex_source_reservation_seal.rs"]
pub(crate) mod seal;
pub use seal::SourceSeal;

#[path = "codex_source_reservation_census.rs"]
mod census;
pub use census::{CensusCursor, CensusPage};
#[path = "codex_source_reservation_cancel.rs"]
mod cancel;
pub use cancel::{CancellationProgress, Cancelled};
#[path = "codex_source_target.rs"]
pub(crate) mod target;
pub use target::{InstalledTarget, TargetConstruction, TargetOpenCleanup, TargetReady};
#[path = "codex_source_use_namespace.rs"]
pub(in crate::harness) mod namespace;

#[cfg(all(unix, any(test, debug_assertions)))]
#[path = "codex_source_proof_use_observation.rs"]
mod proof_observation;
#[cfg(all(unix, any(test, debug_assertions)))]
pub(super) use proof_observation::ProofUses;

const MAX_ACTIVE_USES: usize = 512;
const MAX_MEMBER_USES: usize = 32;
const RECENT_USES: usize = 8;
const MAX_USE_HISTORY: usize = 4096;
const MAX_MEMBER_HISTORY: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UseNonce {
    prefix: u64,
    counter: u64,
}
impl UseNonce {
    pub fn parse(word: &str) -> Option<Self> {
        if !hex(word, 32) {
            return None;
        }
        let value = Self {
            prefix: u64::from_str_radix(&word[..16], 16).ok()?,
            counter: u64::from_str_radix(&word[16..], 16).ok()?,
        };
        (value.counter > 0).then_some(value)
    }
}

pub struct UseRequest<'a> {
    pub member: &'a str,
    pub registration: &'a str,
    pub nonce: UseNonce,
}
pub struct UseReceipt {
    pub capability: String,
}
pub struct UseIdentity<'a> {
    pub member: &'a str,
    pub nonce: UseNonce,
    pub capability: &'a str,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ParentUse {
    member: String,
    nonce: UseNonce,
    capability: String,
}
pub(super) fn parent_use(context: &SourceUse) -> ParentUse {
    ParentUse {
        member: context.member.clone(),
        nonce: context.nonce,
        capability: context.capability.clone(),
    }
}
pub enum UseProgress {
    Active(UseReceipt),
    Closed,
}
/// A pending native intent owns counted custody but no use capability.
pub enum BeginAdmission {
    Pending,
    Settled(UseProgress),
}

#[derive(Clone, PartialEq, Eq)]
pub enum UseAction {
    Begin { registration: String },
    End { capability: String },
    Close { registration: String },
}
/// Already-accounted native work; transport queues must retain this before Waiting.
pub enum UseCommand {
    Begin(Box<RegisteredMemberWork>),
    Cleanup(Box<OwnedUseCleanup>),
    Settled(UseProgress),
}

struct PendingBegin {
    registration: String,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    scope: super::super::account_work::Scope,
}

struct ActiveUse {
    role: ReadyRole,
    registration: String,
    capability: String,
    source: Arc<slot::SourceHandle>,
    gate: super::super::codex_source_gate::SourceGateIdentity,
    _scope: super::super::account_work::Scope,
}
struct EndingUse {
    registration: String,
    capability: Option<String>,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    retirement: super::super::account_work::Cancellation,
}
impl EndingUse {
    fn matches(&self, registration: Option<&str>, capability: Option<&str>) -> bool {
        registration.is_none_or(|value| value == self.registration)
            && capability.is_none_or(|value| self.capability.as_deref() == Some(value))
    }
    fn settled(&self) -> bool {
        self.retirement.retired()
    }
}
struct EndedUse {
    counter: u64,
    registration: String,
    capability: Option<String>,
}
pub(super) struct Uses {
    prefix: u64,
    highwater: u64,
    active: BTreeMap<u64, ActiveUse>,
    pending: BTreeMap<u64, PendingBegin>,
    ending: BTreeMap<u64, EndingUse>,
    recent: std::collections::VecDeque<EndedUse>,
}

/// Resolved from a real active registry record and authenticated peer, not a
/// caller-supplied operation/member string. It owns no last-use retirement.
pub struct SourceUse {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    member: String,
    nonce: UseNonce,
    capability: String,
}

/// Owed work of an already qualified member, not a generic foreground peer or
/// permission to create membership/choose an account. The native front door
/// supplies the actual socket caller and its own App lifetime/scope.
pub struct RegisteredMemberWork {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    member: String,
    registration: String,
    nonce: UseNonce,
    scope: super::super::account_work::Scope,
}

impl std::fmt::Debug for RegisteredMemberWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RegisteredMemberWork(redacted)")
    }
}

/// Release-only identity for a retained member on a dedicated authenticated
/// connection. It cannot become a Peer or authorize source/new-work admission.
pub struct RegisteredMemberCleanup {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    member: String,
    _custody: CleanupCustody,
}
enum CleanupCustody {
    Original {
        _scope: super::super::account_work::Scope,
    },
    Release {
        _turn: super::super::account_work::ReleaseTurn,
    },
}
impl std::fmt::Debug for RegisteredMemberCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RegisteredMemberCleanup(redacted)")
    }
}

/// Exact release action; the native turn cannot be reused for another body.
pub enum UseCleanupRequest<'a> {
    End {
        member: &'a str,
        nonce: UseNonce,
        capability: &'a str,
    },
    Close(UseRequest<'a>),
}
enum CleanupBody {
    End {
        nonce: UseNonce,
        capability: String,
    },
    Close {
        nonce: UseNonce,
        registration: String,
    },
}
pub struct OwnedUseCleanup {
    identity: RegisteredMemberCleanup,
    body: CleanupBody,
}
pub enum CleanupAdmission {
    Mutable(Box<OwnedUseCleanup>),
    /// Pure known-negative correlation. No turn or mutation authority exists.
    Closed,
}
impl std::fmt::Debug for OwnedUseCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedUseCleanup(redacted)")
    }
}
enum CleanupState {
    Original(super::super::account_work::Scope),
    FenceNeeded,
    Closed,
}

fn history_capacity(state: &State, key: &str) -> Result<(), Error> {
    let count = |uses: &Uses| {
        uses.active.len() + uses.pending.len() + uses.ending.len() + uses.recent.len()
    };
    if (!state.uses.contains_key(key) && state.uses.len() >= MAX_USE_HISTORY)
        || state.uses.values().map(count).sum::<usize>() >= MAX_USE_HISTORY
        || state
            .uses
            .get(key)
            .is_some_and(|uses| count(uses) >= MAX_MEMBER_HISTORY)
    {
        return Err(Error::Busy);
    }
    Ok(())
}

fn cleanup_state(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    member: &str,
    body: &CleanupBody,
) -> Result<CleanupState, Error> {
    let retained = retained_member(state, caller, process, member)?;
    let key = member_key(caller, member)?;
    let (nonce, registration, capability) = match body {
        CleanupBody::End { nonce, capability } => (*nonce, None, Some(capability.as_str())),
        CleanupBody::Close {
            nonce,
            registration,
        } => (*nonce, Some(registration.as_str()), None),
    };
    let Some(uses) = state.uses.get(&key) else {
        return if retained && registration.is_some() {
            Ok(CleanupState::FenceNeeded)
        } else {
            Err(Error::Stale)
        };
    };
    if uses.prefix != nonce.prefix {
        return Err(Error::Stale);
    }
    if let Some(ending) = uses.ending.get(&nonce.counter) {
        if !ending.matches(registration, capability) {
            return Err(Error::Stale);
        }
        return if ending.settled() {
            Ok(CleanupState::Closed)
        } else {
            Ok(CleanupState::FenceNeeded)
        };
    }
    if let Some(active) = uses.active.get(&nonce.counter) {
        if !retained {
            return Err(Error::Stale);
        }
        if registration.is_some_and(|value| value != active.registration)
            || capability.is_some_and(|value| value != active.capability)
        {
            return Err(Error::Stale);
        }
        return Ok(CleanupState::Original(active._scope.clone()));
    }
    if let Some(pending) = uses.pending.get(&nonce.counter) {
        if !retained {
            return Err(Error::Stale);
        }
        if registration != Some(pending.registration.as_str()) {
            return Err(Error::Stale);
        }
        return Ok(CleanupState::Original(pending.scope.clone()));
    }
    if let Some(ended) = uses
        .recent
        .iter()
        .find(|ended| ended.counter == nonce.counter)
    {
        if registration.is_some_and(|value| value != ended.registration)
            || capability.is_some_and(|value| ended.capability.as_deref() != Some(value))
        {
            return Err(Error::Stale);
        }
        return Ok(CleanupState::Closed);
    }
    if !retained || nonce.counter <= uses.highwater {
        return Err(Error::Stale);
    }
    if registration.is_some() {
        Ok(CleanupState::FenceNeeded)
    } else {
        Err(Error::Stale)
    }
}

fn retained_member(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    member: &str,
) -> Result<bool, Error> {
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    if actor.process != *process {
        return Err(Error::Stale);
    }
    if actor.issued.contains_key(member) {
        return Ok(true);
    }
    let counter = member.strip_prefix(&actor.epoch).ok_or(Error::Stale)?;
    let counter = u64::from_str_radix(counter, 16).map_err(|_| Error::Stale)?;
    if !hex(member, 32) || counter == 0 || counter > actor.highwater {
        return Err(Error::Stale);
    }
    // Explicit member retirement required all native uses to settle first.
    // This is only a negative observation; it cannot recreate a capability.
    Ok(false)
}

fn loaded_role(member: &Member) -> bool {
    match member.readiness.role {
        Some(ReadyRole::Manager(_)) => true,
        Some(ReadyRole::ReaderLoaded) => {
            member.readiness.reader_owned && member.readiness.gate.is_some()
        }
        _ => false,
    }
}

#[derive(Clone)]
struct LoadedMember {
    process: Process,
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    connection: Connection,
    version: u64,
    registration: String,
    role: Option<ReadyRole>,
    reader_owned: bool,
    gate_held: bool,
}
impl LoadedMember {
    fn capture(member: &Member) -> Result<Self, Error> {
        if member.unknown || !member.connection.is_live() || !loaded_role(member) {
            return Err(Error::Stale);
        }
        Ok(Self {
            process: member.process.clone(),
            source: member.source.clone(),
            gate: member.readiness.identity.clone(),
            connection: member.connection.clone(),
            version: member.version,
            registration: member.registration.clone(),
            role: member.readiness.role,
            reader_owned: member.readiness.reader_owned,
            gate_held: member.readiness.gate.is_some(),
        })
    }
    fn matches(&self, member: &Member) -> bool {
        !member.unknown
            && member.connection.is_live()
            && loaded_role(member)
            && member.process == self.process
            && Arc::ptr_eq(&member.source, &self.source)
            && member.readiness.identity == self.gate
            && member.connection.same(&self.connection)
            && member.version == self.version
            && member.registration == self.registration
            && member.readiness.role == self.role
            && member.readiness.reader_owned == self.reader_owned
            && member.readiness.gate.is_some() == self.gate_held
    }
    fn observed(&self) -> bool {
        self.connection.is_live()
            && Process::observe(self.process.pid).as_ref() == Some(&self.process)
    }
}

// Data-only while State is held. Filesystem/process/scope observations belong
// outside this global critical section, followed by an exact recheck.
fn active_use<'a>(state: &'a State, context: &SourceUse) -> Result<&'a ActiveUse, Error> {
    let key = member_key(&context.caller, &context.member)?;
    let uses = state.uses.get(&key).ok_or(Error::Stale)?;
    if uses.prefix != context.nonce.prefix {
        return Err(Error::Stale);
    }
    let active = uses
        .active
        .get(&context.nonce.counter)
        .ok_or(Error::Stale)?;
    if active.capability != context.capability {
        return Err(Error::Stale);
    }
    let member = state.members.get(&key).ok_or(Error::Stale)?;
    if member.caller != context.caller
        || member.unknown
        || !member.connection.is_live()
        || !loaded_role(member)
        || !same_source_scope(&active.source, &member.source)
        || active.gate != member.readiness.identity
    {
        return Err(Error::Stale);
    }
    Ok(active)
}

fn replay_use(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    request: &UseRequest<'_>,
) -> Result<Option<UseProgress>, Error> {
    let retained = retained_member(state, caller, process, request.member)?;
    let key = member_key(caller, request.member)?;
    let Some(uses) = state.uses.get(&key) else {
        return if retained {
            Ok(None)
        } else {
            Err(Error::Stale)
        };
    };
    if uses.prefix != request.nonce.prefix {
        return Err(Error::Stale);
    }
    if let Some(ending) = uses.ending.get(&request.nonce.counter) {
        if !ending.matches(Some(request.registration), None) {
            return Err(Error::Stale);
        }
        return if ending.settled() {
            Ok(Some(UseProgress::Closed))
        } else {
            Err(Error::Busy)
        };
    }
    if let Some(active) = uses.active.get(&request.nonce.counter) {
        if !retained {
            return Err(Error::Stale);
        }
        if active.registration != request.registration {
            return Err(Error::Stale);
        }
        return Ok(Some(UseProgress::Active(UseReceipt {
            capability: active.capability.clone(),
        })));
    }
    if let Some(pending) = uses.pending.get(&request.nonce.counter) {
        if !retained {
            return Err(Error::Stale);
        }
        return if pending.registration == request.registration {
            Ok(None)
        } else {
            Err(Error::Stale)
        };
    }
    if let Some(ended) = uses
        .recent
        .iter()
        .find(|ended| ended.counter == request.nonce.counter)
    {
        return if ended.registration == request.registration {
            Ok(Some(UseProgress::Closed))
        } else {
            Err(Error::Stale)
        };
    }
    if !retained || request.nonce.counter <= uses.highwater {
        return Err(Error::Stale);
    }
    Ok(None)
}

fn begin_candidate(
    state: &State,
    caller: &slot::Caller,
    process: &Process,
    connection: &Connection,
    request: &UseRequest<'_>,
) -> Result<LoadedMember, Error> {
    let key = member_key(caller, request.member)?;
    let existing = state
        .uses
        .get(&key)
        .is_some_and(|uses| uses.pending.contains_key(&request.nonce.counter));
    if !existing && state.preloads.contains_key(&key) {
        return Err(Error::Busy);
    }
    if !existing
        && (state
            .uses
            .values()
            .map(|uses| {
                uses.active.len()
                    + uses.pending.len()
                    + uses
                        .ending
                        .values()
                        .filter(|ending| !ending.settled())
                        .count()
            })
            .sum::<usize>()
            >= MAX_ACTIVE_USES
            || state.uses.get(&key).is_some_and(|uses| {
                uses.active.len()
                    + uses.pending.len()
                    + uses
                        .ending
                        .values()
                        .filter(|ending| !ending.settled())
                        .count()
                    >= MAX_MEMBER_USES
            }))
    {
        return Err(Error::Busy);
    }
    if !existing {
        history_capacity(state, &key)?;
    }
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    let allocation = actor.issued.get(request.member).ok_or(Error::Stale)?;
    if actor.process != *process || !allocation.connection.same(connection) {
        return Err(Error::Stale);
    }
    reservation_allows_member(state, caller, request.member)?;
    let member = state.members.get(&key).ok_or(Error::Stale)?;
    if member.caller != *caller
        || member.process != *process
        || member.registration != request.registration
        || !member.connection.same(connection)
    {
        return Err(Error::Stale);
    }
    if let Some(pending) = state
        .uses
        .get(&key)
        .and_then(|uses| uses.pending.get(&request.nonce.counter))
        && (!same_source_scope(&pending.source, &member.source)
            || pending.gate != member.readiness.identity)
    {
        return Err(Error::Stale);
    }
    LoadedMember::capture(member)
}

struct UseSnapshot {
    source: Arc<slot::SourceHandle>,
    gate: SourceGateIdentity,
    scope: super::super::account_work::Scope,
    member: LoadedMember,
}
impl UseSnapshot {
    fn capture(state: &State, context: &SourceUse) -> Result<Self, Error> {
        reservation_allows_member(state, &context.caller, &context.member)?;
        let active = active_use(state, context)?;
        let member = state
            .members
            .get(&member_key(&context.caller, &context.member)?)
            .ok_or(Error::Stale)?;
        Ok(Self {
            source: active.source.clone(),
            gate: active.gate.clone(),
            scope: active._scope.clone(),
            member: LoadedMember::capture(member)?,
        })
    }
    fn matches(&self, state: &State, context: &SourceUse) -> Result<(), Error> {
        reservation_allows_member(state, &context.caller, &context.member)?;
        let active = active_use(state, context)?;
        let member = state
            .members
            .get(&member_key(&context.caller, &context.member)?)
            .ok_or(Error::Stale)?;
        if active.gate != self.gate
            || !Arc::ptr_eq(&active.source, &self.source)
            || !self.member.matches(member)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

impl SourceUse {
    #[cfg(all(test, unix))]
    pub(in crate::harness) fn expected_source(
        &self,
        caller: &slot::Caller,
        home: &std::path::Path,
    ) -> Result<Arc<slot::SourceHandle>, Error> {
        self.expected_source_checked(caller, |gate| gate.matches_home(home))
    }

    fn expected_source_checked(
        &self,
        caller: &slot::Caller,
        check_gate: impl FnOnce(&SourceGateIdentity) -> bool,
    ) -> Result<Arc<slot::SourceHandle>, Error> {
        if caller != &self.caller {
            return Err(Error::Stale);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let snapshot = {
            let state = lock(&state)?;
            UseSnapshot::capture(&state, self)?
        };
        // No filesystem/process probe or account-lifetime mutex under State.
        if !check_gate(&snapshot.gate) || !snapshot.member.observed() || snapshot.scope.cancelled()
        {
            return Err(Error::Stale);
        }
        let state = lock(&state)?;
        snapshot.matches(&state, self)?;
        Ok(snapshot.source)
    }

    pub fn current(&self) -> bool {
        self.expected_source_checked(&self.caller, SourceGateIdentity::current)
            .is_ok()
    }
}

impl Registry {
    /// Resolve retained release identity before ordinary App admission. A
    /// sealed drain can return only already-Closed correlation, never a fence.
    pub fn prepare_use_cleanup(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: UseCleanupRequest<'_>,
    ) -> Result<CleanupAdmission, Error> {
        if !connection.is_live() {
            return Err(Error::Stale);
        }
        let (member, body) = match request {
            UseCleanupRequest::End {
                member,
                nonce,
                capability,
            } => {
                if !hex(capability, 64) {
                    return Err(Error::Stale);
                }
                (
                    member,
                    CleanupBody::End {
                        nonce,
                        capability: capability.into(),
                    },
                )
            }
            UseCleanupRequest::Close(request) => {
                if !hex(request.registration, 32) {
                    return Err(Error::Stale);
                }
                (
                    request.member,
                    CleanupBody::Close {
                        nonce: request.nonce,
                        registration: request.registration.into(),
                    },
                )
            }
        };
        let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let (app, first) = {
            let state = lock(&self.state)?;
            (
                state.app_lifetime.clone().ok_or(Error::Denied)?,
                cleanup_state(&state, caller, &process, member, &body)?,
            )
        };
        let custody = match first {
            CleanupState::Closed => return Ok(CleanupAdmission::Closed),
            CleanupState::Original(scope) => CleanupCustody::Original { _scope: scope },
            CleanupState::FenceNeeded => {
                // No Registry mutex while entering the actual App namespace.
                let turn = app.release_turn();
                let current = {
                    let state = lock(&self.state)?;
                    cleanup_state(&state, caller, &process, member, &body)?
                };
                match current {
                    CleanupState::Closed => return Ok(CleanupAdmission::Closed),
                    CleanupState::Original(scope) => CleanupCustody::Original { _scope: scope },
                    CleanupState::FenceNeeded => CleanupCustody::Release {
                        _turn: turn.ok_or(Error::Unavailable)?,
                    },
                }
            }
        };
        if !connection.is_live() || Process::observe(process.pid).as_ref() != Some(&process) {
            return Err(Error::Stale);
        }
        Ok(CleanupAdmission::Mutable(Box::new(OwnedUseCleanup {
            identity: RegisteredMemberCleanup {
                state: Arc::downgrade(&self.state),
                caller: caller.clone(),
                process,
                connection: connection.clone(),
                member: member.into(),
                _custody: custody,
            },
            body,
        })))
    }

    pub fn settle_use_cleanup(&self, cleanup: Box<OwnedUseCleanup>) -> Result<UseProgress, Error> {
        self.check_cleanup(&cleanup.identity)?;
        let OwnedUseCleanup { identity, body } = *cleanup;
        let RegisteredMemberCleanup {
            caller,
            process,
            member,
            _custody,
            ..
        } = identity;
        // The cleanup envelope must not itself keep the original operation
        // alive while asking whether all credential captures have retired.
        let _release = match _custody {
            CleanupCustody::Original { _scope } => {
                drop(_scope);
                None
            }
            CleanupCustody::Release { _turn } => Some(_turn),
        };
        match body {
            CleanupBody::End { nonce, capability } => {
                self.end_checked(&caller, &process, &member, nonce, &capability)?;
                Ok(UseProgress::Closed)
            }
            CleanupBody::Close {
                nonce,
                registration,
            } => self.close_checked(
                &caller,
                &process,
                UseRequest {
                    member: &member,
                    nonce,
                    registration: &registration,
                },
            ),
        }
    }

    pub fn registered_member_cleanup(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        member: &str,
        scope: &super::super::account_work::Scope,
    ) -> Result<RegisteredMemberCleanup, Error> {
        if !connection.is_live() {
            return Err(Error::Stale);
        }
        let app = {
            let state = lock(&self.state)?;
            state.app_lifetime.clone().ok_or(Error::Denied)?
        };
        if !scope.cancellation().owned_by(&app) {
            return Err(Error::Denied);
        }
        // Cleanup may outlive foreground, original work EOF and cancellation.
        // Current kernel process/image and retained native actor still match.
        let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            retained_member(&state, caller, &process, member)?;
        }
        Ok(RegisteredMemberCleanup {
            state: Arc::downgrade(&self.state),
            caller: caller.clone(),
            process,
            connection: connection.clone(),
            member: member.into(),
            _custody: CleanupCustody::Original {
                _scope: scope.clone(),
            },
        })
    }

    fn check_cleanup(&self, cleanup: &RegisteredMemberCleanup) -> Result<(), Error> {
        let state = cleanup.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state)
            || !cleanup.connection.is_live()
            || Process::observe(cleanup.process.pid).as_ref() != Some(&cleanup.process)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }

    pub fn end_registered_use(
        &self,
        cleanup: &RegisteredMemberCleanup,
        nonce: UseNonce,
        capability: &str,
    ) -> Result<(), Error> {
        self.check_cleanup(cleanup)?;
        self.end_checked(
            &cleanup.caller,
            &cleanup.process,
            &cleanup.member,
            nonce,
            capability,
        )
    }

    pub fn close_registered_use(
        &self,
        cleanup: &RegisteredMemberCleanup,
        request: UseRequest<'_>,
    ) -> Result<UseProgress, Error> {
        self.check_cleanup(cleanup)?;
        if request.member != cleanup.member {
            return Err(Error::Stale);
        }
        self.close_checked(&cleanup.caller, &cleanup.process, request)
    }

    pub fn prepare_use_command(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        operation: &str,
        member: &str,
        action: &UseAction,
    ) -> Result<UseCommand, Error> {
        let nonce = UseNonce::parse(operation).ok_or(Error::Denied)?;
        match action {
            UseAction::Begin { registration } => {
                if !connection.is_live() || !hex(registration, 32) {
                    return Err(Error::Denied);
                }
                let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
                if process.birth != caller.birth {
                    return Err(Error::Stale);
                }
                let request = UseRequest {
                    member,
                    registration,
                    nonce,
                };
                let (app, pending) = {
                    let state = lock(&self.state)?;
                    let app = state.app_lifetime.clone().ok_or(Error::Denied)?;
                    if !retained_member(&state, caller, &process, member)? {
                        return Err(Error::Stale);
                    }
                    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
                    if !actor
                        .issued
                        .get(member)
                        .ok_or(Error::Stale)?
                        .connection
                        .same(connection)
                    {
                        return Err(Error::Stale);
                    }
                    if let Some(progress) = replay_use(&state, caller, &process, &request)? {
                        return Ok(UseCommand::Settled(progress));
                    }
                    let pending = state
                        .uses
                        .get(&member_key(caller, member)?)
                        .and_then(|uses| uses.pending.get(&nonce.counter))
                        .map(|pending| pending.scope.clone());
                    (app, pending)
                };
                let scope = match pending {
                    Some(scope) => scope,
                    None => app.enter().ok_or(Error::Busy)?,
                };
                let work = RegisteredMemberWork {
                    state: Arc::downgrade(&self.state),
                    caller: caller.clone(),
                    process,
                    connection: connection.clone(),
                    member: member.into(),
                    registration: registration.clone(),
                    nonce,
                    scope,
                };
                match self.reserve_registered_use(&work)? {
                    BeginAdmission::Pending => Ok(UseCommand::Begin(Box::new(work))),
                    BeginAdmission::Settled(progress) => Ok(UseCommand::Settled(progress)),
                }
            }
            UseAction::End { capability } => match self.prepare_use_cleanup(
                caller,
                connection,
                UseCleanupRequest::End {
                    member,
                    nonce,
                    capability,
                },
            )? {
                CleanupAdmission::Mutable(cleanup) => Ok(UseCommand::Cleanup(cleanup)),
                CleanupAdmission::Closed => Ok(UseCommand::Settled(UseProgress::Closed)),
            },
            UseAction::Close { registration } => match self.prepare_use_cleanup(
                caller,
                connection,
                UseCleanupRequest::Close(UseRequest {
                    member,
                    registration,
                    nonce,
                }),
            )? {
                CleanupAdmission::Mutable(cleanup) => Ok(UseCommand::Cleanup(cleanup)),
                CleanupAdmission::Closed => Ok(UseCommand::Settled(UseProgress::Closed)),
            },
        }
    }

    pub fn execute_use_command(&self, command: UseCommand) -> Result<UseProgress, Error> {
        match command {
            UseCommand::Begin(work) => self.begin_registered_use(*work),
            UseCommand::Cleanup(cleanup) => self.settle_use_cleanup(cleanup),
            UseCommand::Settled(progress) => Ok(progress),
        }
    }

    pub fn registered_member_work(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: UseRequest<'_>,
        scope: &super::super::account_work::Scope,
    ) -> Result<RegisteredMemberWork, Error> {
        if !connection.is_live() || !hex(request.registration, 32) {
            return Err(Error::Denied);
        }
        let app_lifetime = {
            let state = lock(&self.state)?;
            state.app_lifetime.clone().ok_or(Error::Denied)?
        };
        if !scope.cancellation().owned_by(&app_lifetime) {
            return Err(Error::Denied);
        }
        // No foreground requirement: the member was already admitted and the
        // qualified SDK owns the real parent Activity through owed tool work.
        let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            let allocation = actor.issued.get(request.member).ok_or(Error::Stale)?;
            if actor.process != process || !allocation.connection.same(connection) {
                return Err(Error::Stale);
            }
        }
        Ok(RegisteredMemberWork {
            state: Arc::downgrade(&self.state),
            caller: caller.clone(),
            process,
            connection: connection.clone(),
            member: request.member.into(),
            registration: request.registration.into(),
            nonce: request.nonce,
            scope: scope.clone(),
        })
    }

    pub fn reserve_registered_use(
        &self,
        work: &RegisteredMemberWork,
    ) -> Result<BeginAdmission, Error> {
        let retained = work.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&retained, &self.state) {
            return Err(Error::Stale);
        }
        let request = UseRequest {
            member: &work.member,
            registration: &work.registration,
            nonce: work.nonce,
        };
        let candidate = {
            let state = lock(&self.state)?;
            if let Some(replay) = replay_use(&state, &work.caller, &work.process, &request)? {
                return Ok(BeginAdmission::Settled(replay));
            }
            if state
                .uses
                .get(&member_key(&work.caller, &work.member)?)
                .is_some_and(|uses| uses.pending.contains_key(&work.nonce.counter))
            {
                return Ok(BeginAdmission::Pending);
            }
            begin_candidate(
                &state,
                &work.caller,
                &work.process,
                &work.connection,
                &request,
            )?
        };
        let cancelled = work.scope.cancelled();
        let mut state = lock(&self.state)?;
        if let Some(replay) = replay_use(&state, &work.caller, &work.process, &request)? {
            return Ok(BeginAdmission::Settled(replay));
        }
        let key = member_key(&work.caller, &work.member)?;
        if state
            .uses
            .get(&key)
            .is_some_and(|uses| uses.pending.contains_key(&work.nonce.counter))
        {
            return Ok(BeginAdmission::Pending);
        }
        if cancelled {
            return Err(Error::Denied);
        }
        let current = begin_candidate(
            &state,
            &work.caller,
            &work.process,
            &work.connection,
            &request,
        )?;
        if !Arc::ptr_eq(&candidate.source, &current.source)
            || !candidate.matches(state.members.get(&key).ok_or(Error::Stale)?)
        {
            return Err(Error::Busy);
        }
        let uses = state.uses.entry(key).or_insert_with(|| Uses {
            prefix: work.nonce.prefix,
            highwater: 0,
            active: BTreeMap::new(),
            pending: BTreeMap::new(),
            ending: BTreeMap::new(),
            recent: std::collections::VecDeque::new(),
        });
        uses.highwater = work.nonce.counter;
        uses.pending.insert(
            work.nonce.counter,
            PendingBegin {
                registration: work.registration.clone(),
                source: candidate.source,
                gate: candidate.gate,
                scope: work.scope.clone(),
            },
        );
        Ok(BeginAdmission::Pending)
    }

    pub fn begin_registered_use(&self, work: RegisteredMemberWork) -> Result<UseProgress, Error> {
        let state = work.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state) {
            return Err(Error::Stale);
        }
        if let BeginAdmission::Settled(progress) = self.reserve_registered_use(&work)? {
            return Ok(progress);
        }
        self.begin_checked(
            &work.caller,
            &work.process,
            &work.connection,
            UseRequest {
                member: &work.member,
                registration: &work.registration,
                nonce: work.nonce,
            },
            &work.scope,
        )
    }

    /// Recover only an existing derived allocation before asking for fresh
    /// parent-use authority. It cannot allocate, load or retarget a member.
    pub fn resume_derived_open(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
        parent: UseIdentity<'_>,
    ) -> Result<Option<MemberAllocation>, Error> {
        if !peer.current()
            || !connection.is_live()
            || !hex(operation, 32)
            || !hex(parent.capability, 64)
        {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        if !connection.is_live() || !peer.current() {
            return Err(Error::Stale);
        }
        if target::derived_open_is_closing(&state, &peer.caller, operation) {
            return Err(Error::Stale);
        }
        let actor = state
            .actors
            .get_mut(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?;
        if actor.process != peer.process {
            return Err(Error::Stale);
        }
        let Some((member, issued)) = actor
            .issued
            .iter_mut()
            .find(|(_, issued)| issued.operation == operation)
        else {
            return Ok(None);
        };
        let body = ParentUse {
            member: parent.member.into(),
            nonce: parent.nonce,
            capability: parent.capability.into(),
        };
        if issued.parent_use.as_ref() != Some(&body) {
            return Err(Error::Stale);
        }
        if !issued.connection.same(connection) {
            if issued.connection.is_live() {
                return Err(Error::Stale);
            }
            issued.connection = connection.clone();
        }
        Ok(Some(MemberAllocation {
            member: member.clone(),
            operation: operation.into(),
        }))
    }

    /// A new authenticated child connection can allocate only under an exact
    /// still-active parent capability. This returns allocation correlation,
    /// never a generic cross-connection SourceUse or credential-read grant.
    pub fn open_member_derived_from_cap(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
        parent: UseIdentity<'_>,
    ) -> Result<MemberAllocation, Error> {
        if let Some(existing) = self.resume_derived_open(
            peer,
            connection,
            operation,
            UseIdentity {
                member: parent.member,
                nonce: parent.nonce,
                capability: parent.capability,
            },
        )? {
            return Ok(existing);
        }
        let context = SourceUse {
            state: Arc::downgrade(&self.state),
            caller: peer.caller.clone(),
            member: parent.member.into(),
            nonce: parent.nonce,
            capability: parent.capability.into(),
        };
        {
            let state = lock(&self.state)?;
            issued(&state, peer, parent.member)?;
            let active = namespace::admitted(&state, &context)?.0;
            if active._scope.cancelled()
                || state
                    .app_lifetime
                    .as_ref()
                    .is_none_or(|app| !active._scope.cancellation().owned_by(app))
            {
                return Err(Error::Stale);
            }
            reservation_allows_member(&state, &peer.caller, parent.member)?;
        }
        self.open_member_derived(peer, connection, operation, &context)
    }

    pub fn open_member_derived(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
        context: &SourceUse,
    ) -> Result<MemberAllocation, Error> {
        let state = context.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state) {
            return Err(Error::Stale);
        }
        if let Some(existing) = self.resume_derived_open(
            peer,
            connection,
            operation,
            UseIdentity {
                member: &context.member,
                nonce: context.nonce,
                capability: &context.capability,
            },
        )? {
            return Ok(existing);
        }
        context.continuation_current()?;
        if let Some(allocation) =
            self.open_installed_derived(peer, connection, operation, context)?
        {
            return Ok(allocation);
        }
        self.open_member_inner(peer, connection, operation, Some(context))
    }

    /// Fence a cancelled, possibly delayed Begin. An already accepted use is
    /// returned for explicit last-use cleanup; it is never silently ended.
    pub fn close_unaccepted_use(
        &self,
        peer: &Peer,
        connection: &Connection,
        request: UseRequest<'_>,
    ) -> Result<UseProgress, Error> {
        if !peer.current() || !connection.is_live() || !hex(request.registration, 32) {
            return Err(Error::Stale);
        }
        self.close_checked(&peer.caller, &peer.process, request)
    }

    fn close_checked(
        &self,
        caller: &slot::Caller,
        process: &Process,
        request: UseRequest<'_>,
    ) -> Result<UseProgress, Error> {
        if !hex(request.registration, 32) {
            return Err(Error::Stale);
        }
        let key = member_key(caller, request.member)?;
        let mut state = lock(&self.state)?;
        if !retained_member(&state, caller, process, request.member)? {
            return match cleanup_state(
                &state,
                caller,
                process,
                request.member,
                &CleanupBody::Close {
                    nonce: request.nonce,
                    registration: request.registration.into(),
                },
            )? {
                CleanupState::Closed => Ok(UseProgress::Closed),
                _ => Err(Error::Stale),
            };
        }
        if !state.uses.contains_key(&key) {
            history_capacity(&state, &key)?;
        }
        if state.uses.get(&key).is_some_and(|uses| {
            request.nonce.counter > uses.highwater && uses.recent.len() < RECENT_USES
        }) {
            history_capacity(&state, &key)?;
        }
        let uses = state.uses.entry(key).or_insert_with(|| Uses {
            prefix: request.nonce.prefix,
            highwater: 0,
            active: BTreeMap::new(),
            pending: BTreeMap::new(),
            ending: BTreeMap::new(),
            recent: std::collections::VecDeque::new(),
        });
        if uses.prefix != request.nonce.prefix {
            return Err(Error::Stale);
        }
        if let Some(ending) = uses.ending.get(&request.nonce.counter) {
            if !ending.matches(Some(request.registration), None) {
                return Err(Error::Stale);
            }
            return if ending.settled() {
                Ok(UseProgress::Closed)
            } else {
                Err(Error::Busy)
            };
        }
        if let Some(active) = uses.active.get(&request.nonce.counter) {
            if active.registration != request.registration {
                return Err(Error::Stale);
            }
            return Ok(UseProgress::Active(UseReceipt {
                capability: active.capability.clone(),
            }));
        }
        if let Some(pending) = uses.pending.get(&request.nonce.counter) {
            if pending.registration != request.registration {
                return Err(Error::Stale);
            }
            let pending = uses
                .pending
                .remove(&request.nonce.counter)
                .ok_or(Error::Stale)?;
            let retirement = pending.scope.cancellation();
            retirement.cancel();
            uses.ending.insert(
                request.nonce.counter,
                EndingUse {
                    registration: pending.registration.clone(),
                    capability: None,
                    source: pending.source.clone(),
                    gate: pending.gate.clone(),
                    retirement,
                },
            );
            drop(state);
            drop(pending);
            return self.ending_progress(
                caller,
                request.member,
                request.nonce,
                Some(request.registration),
                None,
            );
        }
        if let Some(ended) = uses
            .recent
            .iter()
            .find(|ended| ended.counter == request.nonce.counter)
        {
            return if ended.registration == request.registration {
                Ok(UseProgress::Closed)
            } else {
                Err(Error::Stale)
            };
        }
        if request.nonce.counter <= uses.highwater {
            return Err(Error::Stale);
        }
        uses.highwater = request.nonce.counter;
        uses.recent.push_back(EndedUse {
            counter: request.nonce.counter,
            registration: request.registration.into(),
            capability: None,
        });
        while uses.recent.len() > RECENT_USES {
            uses.recent.pop_front();
        }
        Ok(UseProgress::Closed)
    }

    /// The qualified SDK owns the matching actual root/child Domain Activity.
    /// Allocate its nonce in serialized first-send order, retaining it through
    /// uncertain transport. Native custody persists independently of handles.
    #[cfg(all(test, unix))]
    pub(super) fn begin_use(
        &self,
        peer: Peer,
        connection: &Connection,
        request: UseRequest<'_>,
        scope: &super::super::account_work::Scope,
    ) -> Result<UseProgress, Error> {
        if !peer.current() {
            return Err(Error::Stale);
        }
        self.begin_checked(&peer.caller, &peer.process, connection, request, scope)
    }

    fn begin_checked(
        &self,
        caller: &slot::Caller,
        process: &Process,
        connection: &Connection,
        request: UseRequest<'_>,
        scope: &super::super::account_work::Scope,
    ) -> Result<UseProgress, Error> {
        self.begin_checked_with_gate(
            caller,
            process,
            connection,
            request,
            scope,
            SourceGateIdentity::current,
        )
    }

    fn begin_checked_with_gate(
        &self,
        caller: &slot::Caller,
        process: &Process,
        connection: &Connection,
        request: UseRequest<'_>,
        scope: &super::super::account_work::Scope,
        check_gate: impl FnOnce(&SourceGateIdentity) -> bool,
    ) -> Result<UseProgress, Error> {
        if !connection.is_live() || !hex(request.registration, 32) {
            let pending = {
                let state = lock(&self.state)?;
                state
                    .uses
                    .get(&member_key(caller, request.member)?)
                    .is_some_and(|uses| uses.pending.contains_key(&request.nonce.counter))
            };
            if pending {
                return self.close_checked(caller, process, request);
            }
            return Err(Error::Stale);
        }
        let (candidate, scope) = {
            let state = lock(&self.state)?;
            // Accepted-body recovery is independent of current Ready/gate/scope.
            if let Some(replay) = replay_use(&state, caller, process, &request)? {
                return Ok(replay);
            }
            let candidate = match begin_candidate(&state, caller, process, connection, &request) {
                Ok(candidate) => candidate,
                Err(error) => {
                    let key = member_key(caller, request.member)?;
                    let pending = state
                        .uses
                        .get(&key)
                        .is_some_and(|uses| uses.pending.contains_key(&request.nonce.counter));
                    drop(state);
                    if pending {
                        return self.close_checked(caller, process, request);
                    }
                    return Err(error);
                }
            };
            let scope = state
                .uses
                .get(&member_key(caller, request.member)?)
                .and_then(|uses| uses.pending.get(&request.nonce.counter))
                .map_or_else(|| scope.clone(), |pending| pending.scope.clone());
            (candidate, scope)
        };
        let observation = if scope.cancelled() {
            Err(Error::Denied)
        } else if !candidate.observed() || !check_gate(&candidate.gate) {
            Err(Error::Stale)
        } else {
            aterm_uds::rand::hex_token::<32>().map_err(|_| Error::Unavailable)
        };
        let mut state = lock(&self.state)?;
        // Another worker/cleanup may have admitted or closed this nonce while
        // metadata was inspected. Resolve that exact fact before any refusal.
        if let Some(replay) = replay_use(&state, caller, process, &request)? {
            return Ok(replay);
        }
        let capability = match observation {
            Ok(capability) => capability,
            Err(error) => {
                let key = member_key(caller, request.member)?;
                let pending = state
                    .uses
                    .get(&key)
                    .is_some_and(|uses| uses.pending.contains_key(&request.nonce.counter));
                drop(state);
                if pending {
                    return self.close_checked(caller, process, request);
                }
                return Err(error);
            }
        };
        let current = match begin_candidate(&state, caller, process, connection, &request) {
            Ok(current) => current,
            Err(error) => {
                let key = member_key(caller, request.member)?;
                let pending = state
                    .uses
                    .get(&key)
                    .is_some_and(|uses| uses.pending.contains_key(&request.nonce.counter));
                drop(state);
                if pending {
                    return self.close_checked(caller, process, request);
                }
                return Err(error);
            }
        };
        let member = state
            .members
            .get(&member_key(caller, request.member)?)
            .ok_or(Error::Stale)?;
        if !candidate.matches(member) || !Arc::ptr_eq(&candidate.source, &current.source) {
            return Err(Error::Busy);
        }
        let uses = state
            .uses
            .entry(member_key(caller, request.member)?)
            .or_insert_with(|| Uses {
                prefix: request.nonce.prefix,
                highwater: 0,
                active: BTreeMap::new(),
                pending: BTreeMap::new(),
                ending: BTreeMap::new(),
                recent: std::collections::VecDeque::new(),
            });
        let role = candidate.role.ok_or(Error::Stale)?;
        uses.highwater = uses.highwater.max(request.nonce.counter);
        let pending = uses.pending.remove(&request.nonce.counter);
        uses.active.insert(
            request.nonce.counter,
            ActiveUse {
                role,
                registration: request.registration.into(),
                capability: capability.clone(),
                source: candidate.source,
                gate: candidate.gate,
                _scope: scope.clone(),
            },
        );
        drop(state);
        drop(pending);
        Ok(UseProgress::Active(UseReceipt { capability }))
    }

    pub fn use_context(
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
        let state = lock(&self.state)?;
        issued_on(&state, peer, member, connection)?;
        active_use(&state, &context)?;
        reservation_allows_member(&state, &peer.caller, member)?;
        drop(state);
        if !context.current() {
            return Err(Error::Stale);
        }
        Ok(context)
    }

    /// Acknowledged retirement after the SDK's actual last root/derived use.
    /// The absence of a handle or EOF never invokes this transition.
    pub fn end_use(
        &self,
        peer: &Peer,
        connection: &Connection,
        member: &str,
        nonce: UseNonce,
        capability: &str,
    ) -> Result<(), Error> {
        if !peer.current() || !connection.is_live() || !hex(capability, 64) {
            return Err(Error::Stale);
        }
        self.end_checked(&peer.caller, &peer.process, member, nonce, capability)
    }

    fn ending_progress(
        &self,
        caller: &slot::Caller,
        member: &str,
        nonce: UseNonce,
        registration: Option<&str>,
        capability: Option<&str>,
    ) -> Result<UseProgress, Error> {
        let state = lock(&self.state)?;
        let uses = state
            .uses
            .get(&member_key(caller, member)?)
            .ok_or(Error::Stale)?;
        let ending = uses.ending.get(&nonce.counter).ok_or(Error::Stale)?;
        if uses.prefix != nonce.prefix || !ending.matches(registration, capability) {
            return Err(Error::Stale);
        }
        if ending.settled() {
            Ok(UseProgress::Closed)
        } else {
            Err(Error::Busy)
        }
    }

    fn end_checked(
        &self,
        caller: &slot::Caller,
        process: &Process,
        member: &str,
        nonce: UseNonce,
        capability: &str,
    ) -> Result<(), Error> {
        if !hex(capability, 64) {
            return Err(Error::Stale);
        }
        let key = member_key(caller, member)?;
        let mut state = lock(&self.state)?;
        let retained = retained_member(&state, caller, process, member)?;
        let uses = state.uses.get_mut(&key).ok_or(Error::Stale)?;
        if uses.prefix != nonce.prefix || nonce.counter > uses.highwater {
            return Err(Error::Stale);
        }
        if let Some(ending) = uses.ending.get(&nonce.counter) {
            if !ending.matches(None, Some(capability)) {
                return Err(Error::Stale);
            }
            return if ending.settled() {
                Ok(())
            } else {
                Err(Error::Busy)
            };
        }
        if uses.pending.contains_key(&nonce.counter) {
            return Err(Error::Stale);
        }
        let Some(active) = uses.active.get(&nonce.counter) else {
            return if uses.recent.iter().any(|ended| {
                ended.counter == nonce.counter && ended.capability.as_deref() == Some(capability)
            }) {
                Ok(())
            } else {
                Err(Error::Stale)
            };
        };
        if !retained || active.capability != capability {
            return Err(Error::Stale);
        }
        let active = uses.active.remove(&nonce.counter).ok_or(Error::Stale)?;
        let retirement = active._scope.cancellation();
        retirement.cancel();
        uses.ending.insert(
            nonce.counter,
            EndingUse {
                registration: active.registration,
                capability: Some(active.capability),
                source: active.source,
                gate: active.gate,
                retirement,
            },
        );
        drop(state);
        drop(active._scope);
        self.ending_progress(caller, member, nonce, None, Some(capability))
            .map(|_| ())
    }
}

/// Kernel-proven caller departure ends SDK work, but native credential captures
/// still own the real operation. The caller drops returned scopes outside State.
pub(super) fn retire_gone_uses(
    state: &mut State,
    keys: &[String],
) -> Vec<super::super::account_work::Scope> {
    let mut scopes = Vec::new();
    for key in keys {
        let Some(uses) = state.uses.get_mut(key) else {
            continue;
        };
        for (counter, active) in std::mem::take(&mut uses.active) {
            let retirement = active._scope.cancellation();
            retirement.cancel();
            uses.ending.insert(
                counter,
                EndingUse {
                    registration: active.registration,
                    capability: Some(active.capability),
                    source: active.source,
                    gate: active.gate,
                    retirement,
                },
            );
            scopes.push(active._scope);
        }
        for (counter, pending) in std::mem::take(&mut uses.pending) {
            let retirement = pending.scope.cancellation();
            retirement.cancel();
            uses.ending.insert(
                counter,
                EndingUse {
                    registration: pending.registration,
                    capability: None,
                    source: pending.source,
                    gate: pending.gate,
                    retirement,
                },
            );
            scopes.push(pending.scope);
        }
    }
    scopes
}

pub(super) fn has_active_uses(state: &State, key: &str) -> bool {
    state.preloads.contains_key(key) || has_credential_uses(state, key)
}
pub(super) fn has_credential_uses(state: &State, key: &str) -> bool {
    state.uses.get(key).is_some_and(|uses| {
        !uses.active.is_empty()
            || !uses.pending.is_empty()
            || uses.ending.values().any(|ending| !ending.settled())
    })
}

pub(super) fn admit_derived(
    state: &State,
    peer: &Peer,
    connection: &Connection,
    context: &SourceUse,
) -> Result<(), Error> {
    if context.caller != peer.caller || !connection.is_live() {
        return Err(Error::Stale);
    }
    // The child owns a new authenticated work connection. Its exact parent
    // capability still requires the original parent member/connection alive.
    issued(state, peer, &context.member)?;
    let active = namespace::admitted(state, context)?.0;
    if active._scope.cancelled()
        || state
            .app_lifetime
            .as_ref()
            .is_none_or(|app| !active._scope.cancellation().owned_by(app))
    {
        return Err(Error::Stale);
    }
    reservation_allows_member(state, &peer.caller, &context.member)?;
    if let Some(cell) = state.reservations.get(&actor_key(&peer.caller)) {
        let cell = lock(cell)?;
        if !cell.resource_phase.permits_factory()
            && cell
                .installed
                .as_ref()
                .is_some_and(|installed| same_source_scope(installed.source(), &active.source))
        {
            return Err(Error::Busy);
        }
        // open_member_inner owns State through both allocation and join. Both
        // phase and budget refusal must precede Actor highwater/Issued mutation.
        if !cell.phase.admits() || cell.issued.len() >= MAX_MEMBERS {
            return Err(Error::Busy);
        }
    }
    Ok(())
}

pub(super) fn join_derived(
    state: &State,
    caller: &slot::Caller,
    member: &str,
    operation: &str,
) -> Result<(), Error> {
    let cell = state
        .reservations
        .get(&actor_key(caller))
        .ok_or(Error::Stale)?;
    let mut held = lock(cell)?;
    if let Some(old) = held.issued.get(member) {
        return if old == operation {
            Ok(())
        } else {
            Err(Error::Stale)
        };
    }
    if !held.phase.admits() || held.issued.len() >= MAX_MEMBERS {
        return Err(Error::Busy);
    }
    held.issued.insert(member.into(), operation.into());
    Ok(())
}

// Admission checks are shared by use admission and member continuations.
pub(super) fn fresh_ordinary_preload_allowed(
    state: &State,
    caller: &slot::Caller,
) -> Result<(), Error> {
    if let Some(cell) = state.reservations.get(&actor_key(caller))
        && !matches!(lock(cell)?.phase, seal::Phase::Terminal)
    {
        return Err(Error::Busy);
    }
    Ok(())
}

pub(super) fn reservation_allows_member(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> Result<(), Error> {
    if state
        .preload_sequences
        .get(&member_key(caller, member)?)
        .is_some_and(preload_sequence::Ledger::fenced)
    {
        return Err(Error::Stale);
    }
    if target::normalize::handoff::source_revoked(state, caller, member) {
        return Err(Error::Stale);
    }
    if let Some(reservation) = state.reservations.get(&actor_key(caller)) {
        let reservation = lock(reservation)?;
        if matches!(&reservation.phase, seal::Phase::Terminal)
            && target::normalize::allows_member(state, caller, member)
        {
            return Ok(());
        }
        if (!reservation.phase.allows_member(state, caller, member)
            && !target::allows_member(state, &reservation, caller, member))
            || reservation.quiescent.contains_key(member)
        {
            return Err(Error::Busy);
        }
    }
    Ok(())
}

pub(super) struct Cell {
    target: super::super::codex_assignment::prepare::PreparedTarget,
    origin: origin::Origin,
    process: Process,
    capability: String,
    issued: BTreeMap<String, String>,
    quiescent: BTreeMap<String, Quiescent>,
    phase: seal::Phase,
    target_resource: Option<Arc<crate::harness::codex_source_gate::TargetFamilyLease>>,
    target_driver: Arc<AtomicBool>,
    installed: Option<Arc<target::Installed>>,
    target_issued: BTreeMap<String, String>,
    target_constructors: BTreeMap<String, target::ConstructorEntry>,
    target_retiring: Vec<super::super::account_work::Cancellation>,
    resource_phase: target::resources::Phase,
    _scope: Option<super::super::account_work::Scope>,
    retirement: Option<super::super::account_work::Cancellation>,
    cancelled: Option<Arc<super::super::codex_assignment::prepare::cancel::Cancelled>>,
}

// Fields retire in order: no App count may disappear before the actual native
// resources it accounts for. A terminal Cell keeps no counted custody.
pub(super) struct RetiredCustody {
    _constructors: BTreeMap<String, target::ConstructorEntry>,
    _installed: Option<Arc<target::Installed>>,
    _resource: Option<Arc<crate::harness::codex_source_gate::TargetFamilyLease>>,
    _phase: seal::Phase,
    _scope: Option<super::super::account_work::Scope>,
}

impl Cell {
    fn scope(&self) -> Result<&super::super::account_work::Scope, Error> {
        self._scope.as_ref().ok_or(Error::Stale)
    }

    /// The caller must drop this bundle only after releasing State and Cell.
    /// This extracts custody; it asserts neither a successful assignment nor
    /// permission to release a source freeze without its own native proof.
    pub(super) fn take_custody(&mut self) -> RetiredCustody {
        let scope = self._scope.take();
        if let Some(scope) = &scope {
            self.retirement = Some(scope.cancellation());
        }
        RetiredCustody {
            _constructors: std::mem::take(&mut self.target_constructors),
            _installed: self.installed.take(),
            _resource: self.target_resource.take(),
            _phase: std::mem::replace(&mut self.phase, seal::Phase::Terminal),
            _scope: scope,
        }
    }

    pub(super) fn custody_retired(&self) -> bool {
        self.retirement
            .as_ref()
            .is_some_and(|status| status.retired())
    }
}

struct Quiescent {
    registration: String,
    generation: u64,
    digest: String,
    role: ReadyRole,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CensusCondition {
    Unresolved,
    Unknown,
    Manager,
    Reader,
    Quiescent,
    Retired,
    Disjoint,
}
/// Metadata only. No echo of a census row can mint a reservation or a use.
pub struct CensusMember {
    pub member: String,
    pub registration: Option<String>,
    pub condition: CensusCondition,
    pub credential_generation: Option<u64>,
    pub digest: Option<String>,
    pub auth: Option<AuthState>,
}

pub struct Quiescence<'a> {
    pub member: &'a str,
    pub registration: &'a str,
    pub credential_generation: u64,
    pub digest: &'a str,
    /// None is a direct reader, never a fabricated manager AuthState.
    pub auth: Option<AuthState>,
}

fn same_cell(state: &State, cell: &Arc<Mutex<Cell>>, caller: &slot::Caller) -> Result<(), Error> {
    if !state
        .reservations
        .get(&actor_key(caller))
        .is_some_and(|current| Arc::ptr_eq(current, cell))
    {
        return Err(Error::Stale);
    }
    Ok(())
}

fn checked_cell<'a>(
    state: &'a State,
    cell: &'a Arc<Mutex<Cell>>,
    peer: &Peer,
) -> Result<std::sync::MutexGuard<'a, Cell>, Error> {
    same_cell(state, cell, &peer.caller)?;
    let held = lock(cell)?;
    if held.process != peer.process || held.target.caller() != &peer.caller || !peer.current() {
        return Err(Error::Stale);
    }
    let actor = state
        .actors
        .get(&actor_key(&peer.caller))
        .ok_or(Error::Stale)?;
    if actor
        .issued
        .keys()
        .any(|member| !held.issued.contains_key(member))
    {
        return Err(Error::Stale);
    }
    Ok(held)
}

fn row(state: &State, cell: &Cell, member: &str, operation: &str) -> Result<CensusMember, Error> {
    let mut result = CensusMember {
        member: member.into(),
        registration: None,
        condition: CensusCondition::Unresolved,
        credential_generation: None,
        digest: None,
        auth: None,
    };
    let actor = state
        .actors
        .get(&actor_key(cell.target.caller()))
        .ok_or(Error::Stale)?;
    if actor.process != cell.process {
        return Err(Error::Stale);
    }
    let Some(issued) = actor.issued.get(member) else {
        result.condition = CensusCondition::Retired;
        return Ok(result);
    };
    if issued.operation != operation {
        return Err(Error::Stale);
    }
    let key = member_key(cell.target.caller(), member)?;
    if let Some(unrelated) = state.unrelated.get(&key) {
        result.condition = if unrelated.connection.is_live() && unrelated.process == cell.process {
            CensusCondition::Disjoint
        } else {
            CensusCondition::Unknown
        };
        return Ok(result);
    }
    let Some(current) = state.members.get(&key) else {
        if !issued.connection.is_live() {
            result.condition = CensusCondition::Unknown;
        }
        return Ok(result);
    };
    result.registration = Some(current.registration.clone());
    if current.unknown || !current.connection.is_live() || current.process != cell.process {
        result.condition = CensusCondition::Unknown;
        return Ok(result);
    }
    if current.source.binding() != cell.target.source() {
        result.condition = CensusCondition::Disjoint;
        return Ok(result);
    }
    if !cell.origin.matches_authority(&current.source) {
        result.condition = CensusCondition::Unknown;
        return Ok(result);
    }
    result.credential_generation = Some(current.source.credential_generation());
    result.digest = Some(current.source.digest().into());
    result.auth = match current.readiness.role {
        Some(ReadyRole::Manager(auth)) => Some(auth),
        _ => None,
    };
    result.condition = if current.readiness.manager() {
        CensusCondition::Manager
    } else if current.readiness.reader() {
        CensusCondition::Reader
    } else {
        CensusCondition::Unknown
    };
    if let Some(quiescent) = cell.quiescent.get(member)
        && quiescent.registration == current.registration
        && quiescent.generation == current.source.credential_generation()
        && quiescent.digest == current.source.digest()
        && current.readiness.role == Some(quiescent.role)
        && !has_active_uses(state, &key)
    {
        result.condition = CensusCondition::Quiescent;
    }
    Ok(result)
}

fn reservation_scope(
    state: &State,
    scope: &super::super::account_work::Scope,
) -> Result<(), Error> {
    if !state
        .app_lifetime
        .as_ref()
        .is_some_and(|app| scope.cancellation().owned_by(app))
    {
        return Err(Error::Denied);
    }
    if scope.cancelled() {
        return Err(Error::Stale);
    }
    Ok(())
}

fn reservation_cell_scope(state: &State, cell: &Cell) -> Result<(), Error> {
    reservation_scope(state, cell.scope()?)
}

impl Registry {
    /// Reservation minting occurs only under the real prepared Book/target
    /// callback. All account/owner guards are gone when this method returns.
    pub fn reserve_prepared(
        &self,
        env: &Env,
        source: &ReadySourceProof,
        request: &super::super::codex_assignment::protocol::Request,
        scope: &super::super::account_work::Scope,
    ) -> Result<Reservation, Error> {
        let origin = source.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&origin, &self.state) {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            reservation_scope(&state, scope)?;
        }
        let reservation = super::super::codex_assignment::prepare::with_prepared(
            env,
            source,
            request,
            |prepared| {
                let target = prepared.retain_target();
                let origin = {
                    let state = lock(&self.state).map_err(|_| "source reservation busy")?;
                    let member = state
                        .members
                        .get(&source.key)
                        .ok_or("source origin changed")?;
                    if member.version != source.version
                        || !Arc::ptr_eq(&member.source, &source.source)
                        || !source.alive.load(Ordering::Acquire)
                    {
                        return Err("source origin changed".into());
                    }
                    origin::Origin::capture(member)
                };
                if !origin.current() {
                    return Err("source origin changed".into());
                }
                let mut state = lock(&self.state).map_err(|_| "source reservation busy")?;
                let member = state
                    .members
                    .get(&source.key)
                    .ok_or("source origin changed")?;
                if !origin.matches_member(member) || !source.alive.load(Ordering::Acquire) {
                    return Err("source origin changed".into());
                }
                reservation_scope(&state, scope).map_err(|_| "source reservation scope changed")?;
                let actor_key = actor_key(source.caller());
                if let Some(existing) = state.reservations.get(&actor_key) {
                    let held = lock(existing).map_err(|_| "source reservation busy")?;
                    reservation_cell_scope(&state, &held)
                        .map_err(|_| "source reservation scope changed")?;
                    if !held.target.matches(&target) || !held.origin.same_source(&origin) {
                        return Err("source reservation changed".into());
                    }
                    return Ok(Reservation {
                        state: Arc::downgrade(&self.state),
                        cell: existing.clone(),
                    });
                }
                let actor = state.actors.get(&actor_key).ok_or("source actor changed")?;
                if actor.process != member.process {
                    return Err("source actor changed".into());
                }
                let cell = Arc::new(Mutex::new(Cell {
                    target,
                    origin,
                    process: member.process.clone(),
                    capability: aterm_uds::rand::hex_token::<32>()
                        .map_err(|_| "source reservation unavailable")?,
                    issued: actor
                        .issued
                        .iter()
                        .map(|(member, issued)| (member.clone(), issued.operation.clone()))
                        .collect(),
                    quiescent: BTreeMap::new(),
                    phase: seal::Phase::Draining,
                    target_resource: None,
                    target_driver: Arc::new(false.into()),
                    installed: None,
                    target_issued: BTreeMap::new(),
                    target_constructors: BTreeMap::new(),
                    target_retiring: Vec::new(),
                    resource_phase: target::resources::Phase::Initializing,
                    _scope: Some(scope.clone()),
                    retirement: None,
                    cancelled: None,
                }));
                state.reservations.insert(actor_key, cell.clone());
                Ok(Reservation {
                    state: Arc::downgrade(&self.state),
                    cell,
                })
            },
        )
        .map_err(|error| {
            if error.contains("busy") {
                Error::Busy
            } else {
                Error::Stale
            }
        })?;
        {
            let state = lock(&self.state)?;
            reservation_scope(&state, scope)?;
            let retained = lock(&reservation.cell)?;
            reservation_cell_scope(&state, &retained)?;
        }
        Ok(reservation)
    }
}

impl Reservation {
    pub fn capability(&self, peer: &Peer) -> Result<String, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        Ok(checked_cell(&state, &self.cell, peer)?.capability.clone())
    }
    pub fn revision(&self, peer: &Peer) -> Result<String, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let held = checked_cell(&state, &self.cell, peer)?;
        let bytes = aterm_json::to_vec(&(
            "aterm/codex-reservation-census/v1",
            held.target.caller(),
            held.target.operation(),
            &held.issued,
        ))
        .map_err(|_| Error::Unavailable)?;
        Ok(aterm_codec::hex::encode_lower(
            &aterm_digest::Sha256::digest(bytes),
        ))
    }
    pub fn operation(&self, peer: &Peer) -> Result<String, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        Ok(checked_cell(&state, &self.cell, peer)?
            .target
            .operation()
            .into())
    }
    pub fn census(&self, peer: &Peer) -> Result<Vec<CensusMember>, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let held = checked_cell(&state, &self.cell, peer)?;
        held.issued
            .iter()
            .map(|(member, operation)| row(&state, &held, member, operation))
            .collect()
    }

    /// Native validates the qualified SDK's settled last-use report against
    /// the exact loaded member snapshot; it never fetches owner-latest here.
    pub fn quiesce(
        &self,
        peer: &Peer,
        connection: &Connection,
        report: Quiescence<'_>,
    ) -> Result<(), Error> {
        if !connection.is_live() {
            return Err(Error::Stale);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        issued_on(&state, peer, report.member, connection)?;
        let mut held = checked_cell(&state, &self.cell, peer)?;
        let operation = held.issued.get(report.member).ok_or(Error::Stale)?;
        let observation = row(&state, &held, report.member, operation)?;
        let key = member_key(&peer.caller, report.member)?;
        if !matches!(
            observation.condition,
            CensusCondition::Manager | CensusCondition::Reader | CensusCondition::Quiescent
        ) || observation.registration.as_deref() != Some(report.registration)
            || observation.credential_generation != Some(report.credential_generation)
            || observation.digest.as_deref() != Some(report.digest)
            || observation.auth != report.auth
            || has_active_uses(&state, &key)
        {
            return Err(Error::Stale);
        }
        let role = report
            .auth
            .map_or(ReadyRole::ReaderLoaded, ReadyRole::Manager);
        held.quiescent.insert(
            report.member.into(),
            Quiescent {
                registration: report.registration.into(),
                generation: report.credential_generation,
                digest: report.digest.into(),
                role,
            },
        );
        Ok(())
    }
}

pub struct Reservation {
    state: Weak<Mutex<State>>,
    cell: Arc<Mutex<Cell>>,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceReservation(redacted)")
    }
}
impl std::fmt::Debug for SourceUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceUse(redacted)")
    }
}

impl Registry {
    /// Discovery returns only the already-minted, registry-owned cell for this
    /// actual process. It survives a lost callback/postcheck reply and grants
    /// neither target bytes nor permission to release a freeze.
    pub fn reservation_for_peer(&self, peer: &Peer) -> Result<Option<Reservation>, Error> {
        if !peer.current() {
            return Err(Error::Stale);
        }
        let state = lock(&self.state)?;
        let Some(cell) = state.reservations.get(&actor_key(&peer.caller)) else {
            return Ok(None);
        };
        drop(checked_cell(&state, cell, peer)?);
        Ok(Some(Reservation {
            state: Arc::downgrade(&self.state),
            cell: cell.clone(),
        }))
    }

    pub fn resume_reservation(
        &self,
        peer: &Peer,
        operation: &str,
        capability: &str,
    ) -> Result<Reservation, Error> {
        if !peer.current() || !hex(operation, 32) || !hex(capability, 64) {
            return Err(Error::Stale);
        }
        let state = lock(&self.state)?;
        let actor = actor_key(&peer.caller);
        let historical_key = format!("{actor}:{operation}");
        let current = state
            .reservations
            .get(&actor)
            .filter(|cell| lock(cell).is_ok_and(|cell| cell.target.operation() == operation));
        let cell = current
            .or_else(|| state.retired_reservations.get(&historical_key))
            .ok_or(Error::Stale)?;
        let held = lock(cell)?;
        if held.process != peer.process
            || held.target.caller() != &peer.caller
            || held.target.operation() != operation
            || held.capability != capability
            || (current.is_none()
                && (!matches!(held.phase, seal::Phase::Terminal) || !held.custody_retired()))
        {
            return Err(Error::Stale);
        }
        // Historical handles can only replay their typed terminal receipt.
        // Every work factory still requires the active same_cell and phase.
        Ok(Reservation {
            state: Arc::downgrade(&self.state),
            cell: cell.clone(),
        })
    }

    /// Conservative negative front-door guard. It cannot grant Source access.
    pub fn has_reservation(&self, caller: &slot::Caller) -> bool {
        lock(&self.state).map_or(true, |state| {
            state.reservations.contains_key(&actor_key(caller))
        })
    }
}

#[cfg(all(test, unix))]
#[path = "codex_source_reservation_tests.rs"]
mod tests;
