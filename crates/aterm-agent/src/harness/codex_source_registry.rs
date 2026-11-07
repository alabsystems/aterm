// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live managed-source admission. Registration data is not a grant: source
//! bytes and binding come exclusively from the existing authenticated Slot.
//! No publication or first unmanaged-source adoption authority is minted here.

use super::{
    cli::Env,
    codex_grant_owner as owner, codex_session_slot as slot,
    codex_source_gate::SourceWorkLease,
    native_act::{Image, Process},
    worker,
};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

#[path = "codex_source_preload_custody.rs"]
mod preload_custody;
#[path = "codex_source_reservation.rs"]
mod reservation;
pub(in crate::harness) use reservation::seal::SealingSource;
pub use reservation::{
    BeginAdmission, CensusCondition, CensusMember, CleanupAdmission, OwnedUseCleanup, Quiescence,
    RegisteredMemberCleanup, RegisteredMemberWork, Reservation, SourceSeal, SourceUse, UseAction,
    UseCleanupRequest, UseCommand, UseIdentity, UseNonce, UseProgress, UseReceipt, UseRequest,
};

pub const LAUNCH_ENV: &str = "ATERM_CODEX_SOURCE_LAUNCH";
const MAX_MEMBERS: usize = 128;
const MAX_LAUNCHES: usize = 64;
const MAX_ACTORS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Denied,
    Busy,
    Stale,
    Unavailable,
    Unsupported,
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, Error> {
    mutex.try_lock().map_err(|_| Error::Busy)
}

fn observed(caller: &slot::Caller) -> Result<Process, Error> {
    let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
    if process.birth != caller.birth
        || !owner::consumer_capable(&process, true).map_err(|_| Error::Unavailable)?
    {
        return Err(Error::Unsupported);
    }
    let args = atpkg::caller_shell::process_args(caller.pid).ok_or(Error::Unavailable)?;
    if args.env_var("ATERM_SESSION_ID") != Some(caller.sid.as_str()) {
        return Err(Error::Denied);
    }
    Ok(process)
}

fn gone(process: &Process) -> bool {
    if let Some(now) = Process::observe(process.pid) {
        return now.birth != process.birth;
    }
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(process.pid) else {
            return false;
        };
        if pid <= 1 {
            return false;
        }
        // SAFETY: signal zero observes only this positive kernel PID. An unread
        // process is not dead unless the kernel explicitly reports ESRCH.
        (unsafe { libc::kill(pid, 0) }) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// A qualified kernel peer, admitted through the actual PTY or an owned
/// constructor reservation. No constructor accepts a claimed boolean.
pub struct Peer {
    caller: slot::Caller,
    process: Process,
    launch: Option<String>,
}

impl Peer {
    #[cfg(unix)]
    pub fn foreground(
        caller: &slot::Caller,
        master: std::os::fd::BorrowedFd<'_>,
    ) -> Result<Self, Error> {
        use std::os::fd::AsRawFd as _;
        let process = observed(caller)?;
        let args = atpkg::caller_shell::process_args(caller.pid).ok_or(Error::Unavailable)?;
        // A constructor token always chooses constructor admission. Revoked or
        // unknown permission cannot fall back to ordinary foreground admission.
        if args.env_var(LAUNCH_ENV).is_some() {
            return Err(Error::Denied);
        }
        // SAFETY: the caller owns the borrowed live PTY master for this scope.
        let foreground = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
        if i64::from(foreground) != i64::from(caller.pid)
            || Process::observe(caller.pid).as_ref() != Some(&process)
        {
            return Err(Error::Denied);
        }
        Ok(Self {
            caller: caller.clone(),
            process,
            launch: None,
        })
    }
    /// Recheck this admitted kernel mapping; this is not grant authority.
    pub fn current(&self) -> bool {
        Process::observe(self.caller.pid).as_ref() == Some(&self.process)
    }
    pub fn caller(&self) -> &slot::Caller {
        &self.caller
    }
}

struct InitialCell {
    choice: owner::CanonicalLaunchChoice,
    alive: AtomicBool,
    installing: AtomicBool,
    published: AtomicBool,
}

/// Native first-source authority, held only inside the authenticated owner
/// callback. Neither a path, operation ID nor missing book can construct it.
pub(super) struct InitialSourcePermit {
    cell: Arc<InitialCell>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    startup_nonce: String,
    work: SourceWorkLease,
}
impl InitialSourcePermit {
    pub(super) fn caller(&self) -> &slot::Caller {
        &self.caller
    }
    pub(super) fn binding(&self) -> &owner::GrantBinding {
        self.cell.choice.binding()
    }
    pub(super) fn startup_nonce(&self) -> &str {
        &self.startup_nonce
    }
    pub(super) fn current(&self) -> Result<(), Error> {
        if !self.cell.alive.load(Ordering::Acquire)
            || !self.cell.installing.load(Ordering::Acquire)
            || !self.connection.is_live()
            || Process::observe(self.caller.pid).as_ref() != Some(&self.process)
        {
            return Err(Error::Stale);
        }
        self.work.current().map_err(|_| Error::Stale)?;
        if !self.connection.is_live() || !self.cell.alive.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub(super) fn require_unpublished_startup(&self) -> Result<(), Error> {
        self.current()?;
        if self.cell.published.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub(super) fn note_startup_published(&self) {
        // Tighten before any later fallible I/O, even after connection loss.
        self.cell.published.store(true, Ordering::Release);
    }
}
impl Drop for InitialSourcePermit {
    fn drop(&mut self) {
        self.cell.installing.store(false, Ordering::Release);
    }
}

struct Launch {
    sid: String,
    image: Image,
    expected_env: Vec<(String, String)>,
    expected_unset: Vec<&'static str>,
    process: Option<Process>,
    typed: bool,
    gate: Option<SourceWorkLease>,
    _scope: Option<super::account_work::Scope>,
    initial: Option<Arc<InitialCell>>,
}

impl Drop for Launch {
    fn drop(&mut self) {
        if let Some(initial) = &self.initial {
            initial.alive.store(false, Ordering::Release);
        }
    }
}

impl Launch {
    fn bind(&mut self, process: Process) {
        self.process = Some(process);
        self.typed = true;
        // Native constructor work is complete at actual kernel bind. Consumer
        // and connection lifetime now owns startup; the gate is NOT released.
        // This scope never contains an irreversible provider response.
        self._scope.take();
    }
}

/// Real constructor custody. A typed command keeps the gate until actual child
/// death or a later verified source-ready transition, never a task-result line.
pub struct LaunchReservation {
    token: String,
    executable: std::path::PathBuf,
    state: Weak<Mutex<State>>,
}
impl LaunchReservation {
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Build the actual launch from the admitted starter, bypassing the mutable
    /// managed twin and retaining the ordinary sign-in recheck.
    pub fn arm(
        &self,
        admitted: worker::Admitted,
        cli: &worker::WorkerCli,
        home: Option<&std::path::Path>,
    ) -> Result<worker::Admitted, Error> {
        let check = || {
            let state = self.state.upgrade().ok_or(Error::Stale)?;
            let state = lock(&state)?;
            let launch = state.launches.get(&self.token).ok_or(Error::Stale)?;
            if launch.typed
                || launch.process.is_some()
                || Image::at(&self.executable).as_ref() != Some(&launch.image)
            {
                return Err(Error::Stale);
            }
            launch
                .gate
                .as_ref()
                .ok_or(Error::Stale)?
                .current()
                .map_err(|_| Error::Stale)
        };
        check()?;
        let (admitted, ()) = admitted
            .prepare(cli, home, |mut starter| {
                starter.exe = self.executable.clone();
                starter.env.retain(|(name, _)| name != LAUNCH_ENV);
                starter.env.push((LAUNCH_ENV.into(), self.token.clone()));
                starter.unset.retain(|name| *name != LAUNCH_ENV);
                Ok((starter, ()))
            })
            .map_err(|_| Error::Unavailable)?;
        check()?;
        Ok(admitted)
    }
    /// Recheck the same pending constructor immediately before its guarded write.
    pub fn check_for_write(&self, admitted: &worker::Admitted) -> Result<(), Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let state = lock(&state)?;
        let launch = state.launches.get(&self.token).ok_or(Error::Stale)?;
        let starter = admitted.starter();
        let env: Vec<_> = starter
            .env
            .iter()
            .filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "CODEX_HOME" | "CODEX_AUTH_PATH" | "CODEX_AUTH_SLOT"
                )
            })
            .cloned()
            .collect();
        let unset: Vec<_> = starter
            .unset
            .iter()
            .copied()
            .filter(|name| matches!(*name, "CODEX_HOME" | "CODEX_AUTH_PATH" | "CODEX_AUTH_SLOT"))
            .collect();
        if launch.typed
            || launch.process.is_some()
            || Image::at(&starter.exe).as_ref() != Some(&launch.image)
            || env != launch.expected_env
            || unset != launch.expected_unset
            || starter
                .env
                .iter()
                .rev()
                .find(|(key, _)| key == LAUNCH_ENV)
                .map(|(_, value)| value.as_str())
                != Some(self.token.as_str())
        {
            return Err(Error::Stale);
        }
        launch
            .gate
            .as_ref()
            .ok_or(Error::Stale)?
            .current()
            .map_err(|_| Error::Stale)
    }
    /// Record an observed write from the owned launcher worker, never the GUI thread.
    pub fn mark_typed(&self) -> Result<(), Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        state
            .lock()
            .map_err(|_| Error::Unavailable)?
            .launches
            .get_mut(&self.token)
            .ok_or(Error::Stale)?
            .typed = true;
        Ok(())
    }
    /// Revoke an unclaimed constructor permission, even after its command was
    /// typed. A late SourceV1 child must fail before loading any credentials.
    /// This does not assert that a process exited or that its task completed.
    pub fn revoke_unclaimed(self) -> Result<(), Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        // Owned launcher cleanup runs off the GUI thread and must not lose a
        // revocation merely because a short registry transition is in flight.
        let mut state = state.lock().map_err(|_| Error::Unavailable)?;
        if state
            .launches
            .get(&self.token)
            .ok_or(Error::Stale)?
            .process
            .is_some()
        {
            return Err(Error::Denied);
        }
        state.launches.remove(&self.token);
        Ok(())
    }
    pub fn cancel_untyped(self) -> Result<(), Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let mut state = lock(&state)?;
        if state.launches.get(&self.token).ok_or(Error::Stale)?.typed {
            return Err(Error::Denied);
        }
        state.launches.remove(&self.token);
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AuthState {
    pub generation: u64,
    pub owner: u64,
    pub source: u64,
    pub login: u64,
}

/// Actual selected SDK scope with no native OAuth reader. Raw ChatGPT
/// file/keyring sources are deliberately absent; they remain unmanaged/unknown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoNativeReaderKind {
    ApiKey,
    External,
    Workload,
    BedrockApiKey,
    BedrockAccessKeys,
    Headers,
    AgentIdentity,
    PersonalAccessToken,
    NoCredentials,
}

struct UnrelatedMember {
    process: Process,
    operation: String,
    registration: String,
    connection: Connection,
    kind: NoNativeReaderKind,
}

pub enum Ready {
    Loaded {
        credential_generation: u64,
        digest: String,
        account: String,
        user: String,
        auth: AuthState,
    },
    /// A direct storage reader has no AuthManager counters. Its source-use
    /// lease survives READY and EOF until explicit last-use retirement/death.
    ReaderLoaded {
        credential_generation: u64,
        digest: String,
        account: String,
        user: String,
    },
    PolicyUnavailable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadyRole {
    Manager(AuthState),
    ReaderLoaded,
    PolicyUnavailable,
}

struct SourceReadiness {
    gate: Option<SourceWorkLease>,
    identity: super::codex_source_gate::SourceGateIdentity,
    role: Option<ReadyRole>,
    // Reconnect does not retire any earlier direct-reader/derived use.
    reader_owned: bool,
}
impl SourceReadiness {
    fn preload(gate: SourceWorkLease, previous: Option<&mut Self>) -> Result<Self, Error> {
        let new_identity = gate.identity().map_err(|_| Error::Stale)?;
        if previous
            .as_ref()
            .is_some_and(|old| old.identity != new_identity)
        {
            return Err(Error::Stale);
        }
        let identity = if let Some(old) = previous.as_ref().filter(|old| old.reader_owned) {
            old.gate
                .as_ref()
                .ok_or(Error::Stale)?
                .current()
                .map_err(|_| Error::Stale)?;
            old.identity.clone()
        } else {
            new_identity
        };
        let previous_reader = previous.and_then(|old| {
            if old.reader_owned {
                old.gate.take()
            } else {
                None
            }
        });
        let reader_owned = previous_reader.is_some();
        let gate = previous_reader.unwrap_or(gate);
        // All fallible checks precede transferring an old reader's held lock.
        Ok(Self {
            gate: Some(gate),
            identity,
            role: None,
            reader_owned,
        })
    }
    fn complete(&mut self, role: ReadyRole) -> Result<(), Error> {
        if (self.reader_owned && role != ReadyRole::ReaderLoaded)
            || self.role.is_some_and(|previous| previous != role)
        {
            return Err(Error::Stale);
        }
        if let Some(gate) = &self.gate {
            gate.current().map_err(|_| Error::Stale)?;
        } else if role == ReadyRole::ReaderLoaded {
            return Err(Error::Stale);
        }
        self.role = Some(role);
        self.reader_owned |= role == ReadyRole::ReaderLoaded;
        if role != ReadyRole::ReaderLoaded {
            self.gate.take();
        }
        Ok(())
    }
    fn manager(&self) -> bool {
        matches!(self.role, Some(ReadyRole::Manager(_)))
    }
    fn reader(&self) -> bool {
        self.role == Some(ReadyRole::ReaderLoaded)
            && self
                .gate
                .as_ref()
                .is_some_and(|gate| gate.current().is_ok())
    }
}

struct Member {
    caller: slot::Caller,
    process: Process,
    operation: String,
    registration: String,
    connection: Connection,
    version: u64,
    source: Arc<slot::SourceHandle>,
    readiness: SourceReadiness,
    unknown: bool,
}

struct Issued {
    operation: String,
    connection: Connection,
    parent_use: Option<reservation::ParentUse>,
}
struct Actor {
    process: Process,
    epoch: String,
    highwater: u64,
    issued: BTreeMap<String, Issued>,
}
pub struct MemberAllocation {
    pub member: String,
    pub operation: String,
}

#[derive(Default)]
struct State {
    app_lifetime: Option<super::account_work::Lifetime>,
    version: u64,
    members: BTreeMap<String, Member>,
    unrelated: BTreeMap<String, UnrelatedMember>,
    launches: BTreeMap<String, Launch>,
    actors: BTreeMap<String, Actor>,
    uses: BTreeMap<String, reservation::Uses>,
    preloads: BTreeMap<String, Arc<preload_custody::Pending>>,
    reservations: BTreeMap<String, Arc<Mutex<reservation::Cell>>>,
}
#[derive(Default)]
pub struct Registry {
    state: Arc<Mutex<State>>,
}

/// Created synchronously by the authenticated front door before spawning work.
/// EOF invalidates this atomic token even while the member table is busy.
#[derive(Clone)]
pub struct Connection {
    id: u64,
    alive: Arc<AtomicBool>,
}
impl Connection {
    pub fn is_live(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    fn same(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.alive, &other.alive)
    }
}
static CONNECTIONS: std::sync::OnceLock<Mutex<BTreeMap<u64, Arc<AtomicBool>>>> =
    std::sync::OnceLock::new();
fn connections() -> &'static Mutex<BTreeMap<u64, Arc<AtomicBool>>> {
    CONNECTIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn member_key(caller: &slot::Caller, member: &str) -> Result<String, Error> {
    if !hex(member, 32) {
        return Err(Error::Denied);
    }
    Ok(format!(
        "{}:{}:{}:{member}",
        caller.sid, caller.pid, caller.birth
    ))
}

fn actor_key(caller: &slot::Caller) -> String {
    format!("{}:{}:{}", caller.sid, caller.pid, caller.birth)
}
fn issued(state: &State, peer: &Peer, member: &str) -> Result<(), Error> {
    let actor = state
        .actors
        .get(&actor_key(&peer.caller))
        .ok_or(Error::Stale)?;
    if actor.process != peer.process || !actor.issued.contains_key(member) {
        return Err(Error::Stale);
    }
    Ok(())
}

fn issued_on(
    state: &State,
    peer: &Peer,
    member: &str,
    connection: &Connection,
) -> Result<(), Error> {
    issued(state, peer, member)?;
    let allocation = state
        .actors
        .get(&actor_key(&peer.caller))
        .and_then(|actor| actor.issued.get(member))
        .ok_or(Error::Stale)?;
    if !allocation.connection.same(connection) {
        return Err(Error::Stale);
    }
    Ok(())
}

fn version(state: &mut State) -> Result<u64, Error> {
    state.version = state.version.checked_add(1).ok_or(Error::Unavailable)?;
    Ok(state.version)
}

pub(super) fn immutable_binding_hash(binding: &owner::GrantBinding) -> Result<String, Error> {
    // Producer-grant identity deliberately excludes the later account writer's
    // generation. Authority still uses the complete authenticated GrantBinding.
    let bytes = aterm_secrets::SecretBytes::new(
        aterm_json::to_vec(&(
            "aterm/codex-immutable-grant-binding/v1",
            &binding.request,
            &binding.reference,
        ))
        .map_err(|_| Error::Unavailable)?,
    );
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(bytes.as_slice()),
    ))
}

// A grant may be reused by a later session assignment. Its issuance alone
// cannot authorize relabelling existing direct-reader/derived use leases.
fn same_source_scope(left: &slot::SourceHandle, right: &slot::SourceHandle) -> bool {
    left.binding() == right.binding()
        && left.slot() == right.slot()
        && left.path() == right.path()
        && left.assignment_generation() == right.assignment_generation()
        && left.source_authority() == right.source_authority()
        && left.namespaces() == right.namespaces()
        && left.account() == right.account()
        && left.user() == right.user()
}

fn revision_descends(next: u64, next_digest: &str, old: u64, old_digest: &str) -> bool {
    next >= old && (next > old || next_digest == old_digest)
}

pub struct Preload {
    pub member: String,
    pub operation: String,
    pub registration: String,
    source: Arc<slot::SourceHandle>,
}
impl Preload {
    pub fn source(&self) -> &slot::SourceHandle {
        &self.source
    }
    pub fn grant_binding_hash(&self) -> Result<String, Error> {
        immutable_binding_hash(self.source.binding())
    }
}

/// Descriptive proof for ONE live Ready member, not proof of process-cohort or
/// publication completeness. A separate actual actor-freeze/version guard is
/// required before assignment. Its binding includes the real publication G.
pub struct ReadySourceProof {
    state: Weak<Mutex<State>>,
    key: String,
    version: u64,
    caller: slot::Caller,
    source: Arc<slot::SourceHandle>,
    gate_identity: super::codex_source_gate::SourceGateIdentity,
    alive: Arc<AtomicBool>,
}
impl ReadySourceProof {
    pub fn current(&self) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        let Ok(state) = lock(&state) else {
            return false;
        };
        let current = state.members.get(&self.key).is_some_and(|member| {
            member.version == self.version
                && member.readiness.manager()
                && !member.unknown
                && Process::observe(member.caller.pid).as_ref() == Some(&member.process)
        });
        current && self.alive.load(Ordering::Acquire)
    }
    pub fn caller(&self) -> &slot::Caller {
        &self.caller
    }
    pub(super) fn gate_source(
        &self,
    ) -> (
        &slot::Caller,
        u64,
        &slot::SourceHandle,
        &super::codex_source_gate::SourceGateIdentity,
    ) {
        (
            &self.caller,
            self.version,
            &self.source,
            &self.gate_identity,
        )
    }
    pub fn account(&self) -> &str {
        self.source.account()
    }
    pub fn user(&self) -> &str {
        self.source.user()
    }
    pub fn grant_binding_hash(&self) -> Result<String, Error> {
        immutable_binding_hash(self.source.binding())
    }
}

/// Descriptive direct-reader source proof, deliberately distinct from an
/// AuthManager readiness stamp. Not a complete actor/publication freeze.
pub struct ReaderSourceProof {
    state: Weak<Mutex<State>>,
    key: String,
    version: u64,
    caller: slot::Caller,
    source: Arc<slot::SourceHandle>,
    gate_identity: super::codex_source_gate::SourceGateIdentity,
    alive: Arc<AtomicBool>,
}
impl ReaderSourceProof {
    pub fn current(&self) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        let Ok(state) = lock(&state) else {
            return false;
        };
        let current = state.members.get(&self.key).is_some_and(|member| {
            member.version == self.version
                && member.readiness.reader()
                && !member.unknown
                && Process::observe(member.caller.pid).as_ref() == Some(&member.process)
        });
        current && self.alive.load(Ordering::Acquire)
    }
    pub fn caller(&self) -> &slot::Caller {
        &self.caller
    }
    pub(super) fn gate_source(
        &self,
    ) -> (
        &slot::Caller,
        u64,
        &slot::SourceHandle,
        &super::codex_source_gate::SourceGateIdentity,
    ) {
        (
            &self.caller,
            self.version,
            &self.source,
            &self.gate_identity,
        )
    }
    pub fn account(&self) -> &str {
        self.source.account()
    }
    pub fn user(&self) -> &str {
        self.source.user()
    }
    pub fn grant_binding_hash(&self) -> Result<String, Error> {
        immutable_binding_hash(self.source.binding())
    }
}

impl Registry {
    /// Bind this native instance to its actual App lifetime namespace. This
    /// clone counts no operation and cannot be rebound by a client message.
    pub fn for_app(lifetime: &super::account_work::Lifetime) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                app_lifetime: Some(lifetime.clone()),
                ..State::default()
            })),
        }
    }

    /// Native-issued IDs retain a process epoch and monotone ordinal. Only the
    /// active set is bounded; retired IDs can never reopen a source registration.
    pub fn open_member(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
    ) -> Result<MemberAllocation, Error> {
        self.open_member_inner(peer, connection, operation, None)
    }

    fn open_member_inner(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
        context: Option<&SourceUse>,
    ) -> Result<MemberAllocation, Error> {
        if !peer.current() || !hex(operation, 32) || !connection.alive.load(Ordering::Acquire) {
            return Err(Error::Denied);
        }
        self.reap_gone();
        let key = actor_key(&peer.caller);
        let mut state = lock(&self.state)?;
        if !connection.is_live() || !peer.current() {
            return Err(Error::Stale);
        }
        if !state.actors.contains_key(&key) {
            if state.actors.len() >= MAX_ACTORS {
                return Err(Error::Busy);
            }
            state.actors.insert(
                key.clone(),
                Actor {
                    process: peer.process.clone(),
                    epoch: aterm_uds::rand::hex_token::<8>().map_err(|_| Error::Unavailable)?,
                    highwater: 0,
                    issued: BTreeMap::new(),
                },
            );
        }
        let closed = state.reservations.contains_key(&key);
        let parent_use = context.map(reservation::parent_use);
        let actor = state.actors.get_mut(&key).ok_or(Error::Unavailable)?;
        if actor.process != peer.process {
            return Err(Error::Stale);
        }
        if let Some((member, issued)) = actor
            .issued
            .iter_mut()
            .find(|(_, issued)| issued.operation == operation)
        {
            if issued.parent_use != parent_use {
                return Err(Error::Stale);
            }
            if !issued.connection.same(connection) {
                if issued.connection.alive.load(Ordering::Acquire) {
                    return Err(Error::Stale);
                }
                issued.connection = connection.clone();
            }
            return Ok(MemberAllocation {
                member: member.clone(),
                operation: operation.into(),
            });
        }
        // Exact pre-reserve open nonces above may reconcile. A delayed,
        // transport-only request cannot allocate after native closure.
        if closed && context.is_none() {
            return Err(Error::Busy);
        }
        if actor.issued.len() >= MAX_MEMBERS {
            return Err(Error::Busy);
        }
        if let Some(context) = context {
            reservation::admit_derived(&state, peer, connection, context)?;
        }
        let actor = state.actors.get_mut(&key).ok_or(Error::Unavailable)?;
        actor.highwater = actor.highwater.checked_add(1).ok_or(Error::Unavailable)?;
        let member = format!("{}{:016x}", actor.epoch, actor.highwater);
        actor.issued.insert(
            member.clone(),
            Issued {
                operation: operation.into(),
                connection: connection.clone(),
                parent_use,
            },
        );
        if closed {
            reservation::join_derived(&state, &peer.caller, &member, operation)?;
        }
        Ok(MemberAllocation {
            member,
            operation: operation.into(),
        })
    }

    /// Explicit SDK retirement after the last credential-use/HTTP/WS lease.
    /// Neither EOF nor a disappearing weak AuthManager calls this method.
    pub fn retire_member(
        &self,
        peer: Peer,
        connection: &Connection,
        member: &str,
        registration: Option<&str>,
    ) -> Result<(), Error> {
        if !peer.current() || !connection.alive.load(Ordering::Acquire) {
            return Err(Error::Denied);
        }
        let key = member_key(&peer.caller, member)?;
        let mut state = lock(&self.state)?;
        if !peer.current() || !connection.is_live() {
            return Err(Error::Stale);
        }
        if reservation::has_credential_uses(&state, &key) || preload_custody::running(&state, &key)
        {
            return Err(Error::Busy);
        }
        let actor = state
            .actors
            .get(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?;
        if actor.process != peer.process {
            return Err(Error::Stale);
        }
        let Some(allocation) = actor.issued.get(member) else {
            let suffix = member.strip_prefix(&actor.epoch).ok_or(Error::Stale)?;
            let ordinal = u64::from_str_radix(suffix, 16).map_err(|_| Error::Stale)?;
            return if ordinal > 0 && ordinal <= actor.highwater {
                Ok(())
            } else {
                Err(Error::Stale)
            };
        };
        if let Some(current) = state.unrelated.get(&key) {
            if current.process != peer.process
                || !current.connection.same(connection)
                || registration != Some(current.registration.as_str())
            {
                return Err(Error::Stale);
            }
        } else if let Some(current) = state.members.get(&key) {
            if !current.connection.same(connection)
                || registration != Some(current.registration.as_str())
            {
                return Err(Error::Stale);
            }
        } else if registration.is_some() || !allocation.connection.same(connection) {
            return Err(Error::Stale);
        }
        let preload = state.preloads.remove(&key);
        state.uses.remove(&key);
        state.members.remove(&key);
        state.unrelated.remove(&key);
        state
            .actors
            .get_mut(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?
            .issued
            .remove(member);
        version(&mut state)?;
        drop(state);
        drop(preload);
        Ok(())
    }

    /// Create/look up only while the actual socket tenure is synchronously
    /// owned by dispatch, before scheduling any background work. Such work
    /// retains the returned token; it must never recreate one from a numeric ID.
    pub fn connection(&self, id: u64) -> Result<Connection, Error> {
        if id == 0 {
            return Err(Error::Denied);
        }
        let mut connections = connections().lock().map_err(|_| Error::Unavailable)?;
        if connections.len() >= 256 && !connections.contains_key(&id) {
            return Err(Error::Busy);
        }
        let alive = connections
            .entry(id)
            .or_insert_with(|| Arc::new(AtomicBool::new(true)))
            .clone();
        if !alive.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        Ok(Connection { id, alive })
    }
    /// Complete a validated pre-load registration whose actual SDK-selected
    /// cache/provider never reads a native OAuth source. This releases only the
    /// constructor exclusion; it mints no Source/Slot/Grant authority.
    pub fn complete_unrelated(
        &self,
        peer: Peer,
        connection: &Connection,
        member: &str,
        operation: &str,
        kind: NoNativeReaderKind,
    ) -> Result<String, Error> {
        if !peer.current() || !connection.alive.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        let key = member_key(&peer.caller, member)?;
        let mut state = lock(&self.state)?;
        if !connection.is_live() || !peer.current() {
            return Err(Error::Stale);
        }
        issued(&state, &peer, member)?;
        let allocation = state
            .actors
            .get(&actor_key(&peer.caller))
            .and_then(|actor| actor.issued.get(member))
            .ok_or(Error::Stale)?;
        if allocation.operation != operation
            || !allocation.connection.same(connection)
            || state.members.contains_key(&key)
        {
            return Err(Error::Stale);
        }
        if let Some(old) = state.unrelated.get(&key) {
            if old.process != peer.process || old.operation != operation || old.kind != kind {
                return Err(Error::Stale);
            }
            if old.connection.same(connection) {
                return Ok(old.registration.clone());
            }
            if old.connection.alive.load(Ordering::Acquire) {
                return Err(Error::Stale);
            }
        }
        if let Some(token) = &peer.launch {
            let launch = state.launches.get(token).ok_or(Error::Stale)?;
            if launch.process.as_ref() != Some(&peer.process) {
                return Err(Error::Stale);
            }
            if let Some(gate) = &launch.gate {
                gate.current().map_err(|_| Error::Stale)?;
            }
        }
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        state.unrelated.insert(
            key,
            UnrelatedMember {
                process: peer.process,
                operation: operation.into(),
                registration: registration.clone(),
                connection: connection.clone(),
                kind,
            },
        );
        if let Some(token) = peer.launch
            && let Some(launch) = state.launches.get_mut(&token)
        {
            if let Some(initial) = &launch.initial {
                initial.alive.store(false, Ordering::Release);
            }
            launch.gate.take();
            launch._scope.take();
        }
        Ok(registration)
    }

    pub fn reserve_worker(
        &self,
        env: &Env,
        sid: &str,
        admitted: &worker::Admitted,
        scope: &super::account_work::Scope,
    ) -> Result<LaunchReservation, Error> {
        if scope.cancelled() {
            return Err(Error::Denied);
        }
        if !sid.starts_with("s-") || sid.len() > 64 {
            return Err(Error::Denied);
        }
        // Resolve only the existing managed-twin grammar. The kernel observes
        // the package executable, never the wrapper script's inode.
        let selected = &admitted.starter().exe;
        let executable = if super::codex_alab::alab_bytes(selected) {
            selected.clone()
        } else {
            super::upgrade_drive::twin_build(selected, "codex").ok_or(Error::Unsupported)?
        };
        if !super::codex_alab::alab_bytes(&executable)
            || !super::codex_alab::managed_credential_capable(&executable, true)
                .map_err(|_| Error::Unavailable)?
        {
            return Err(Error::Unsupported);
        }
        let image = Image::at(&executable).ok_or(Error::Unavailable)?;
        let executable = std::fs::canonicalize(executable).map_err(|_| Error::Unavailable)?;
        self.reap_gone();
        let gate = SourceWorkLease::for_home(env.home.as_deref().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Busy)?;
        let initial = match worker::declared_codex_auth_file(
            admitted.starter(),
            env.home.as_deref(),
            &env.cwd,
        ) {
            Some(auth) => owner::capture_canonical_launch_choice(env, &auth, &gate)
                .map_err(|_| Error::Unavailable)?
                .map(|choice| {
                    Arc::new(InitialCell {
                        choice,
                        alive: AtomicBool::new(true),
                        installing: AtomicBool::new(false),
                        published: AtomicBool::new(false),
                    })
                }),
            None => None,
        };
        let token = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let mut state = lock(&self.state)?;
        // The App epoch may have closed during executable/profile observation.
        // A cancelled worker must not insert after a drain sweep.
        if scope.cancelled() {
            return Err(Error::Denied);
        }
        if state.launches.len() >= MAX_LAUNCHES {
            return Err(Error::Busy);
        }
        state.launches.insert(
            token.clone(),
            Launch {
                sid: sid.into(),
                image,
                expected_env: admitted
                    .starter()
                    .env
                    .iter()
                    .filter(|(name, _)| {
                        matches!(
                            name.as_str(),
                            "CODEX_HOME" | "CODEX_AUTH_PATH" | "CODEX_AUTH_SLOT"
                        )
                    })
                    .cloned()
                    .collect(),
                expected_unset: admitted
                    .starter()
                    .unset
                    .iter()
                    .copied()
                    .filter(|name| {
                        matches!(*name, "CODEX_HOME" | "CODEX_AUTH_PATH" | "CODEX_AUTH_SLOT")
                    })
                    .collect(),
                process: None,
                typed: false,
                gate: Some(gate),
                _scope: Some(scope.clone()),
                initial,
            },
        );
        Ok(LaunchReservation {
            token,
            executable,
            state: Arc::downgrade(&self.state),
        })
    }

    pub fn reserved_peer(&self, caller: &slot::Caller, token: &str) -> Result<Peer, Error> {
        let process = observed(caller)?;
        let args = atpkg::caller_shell::process_args(caller.pid).ok_or(Error::Unavailable)?;
        if args.env_var(LAUNCH_ENV) != Some(token) {
            return Err(Error::Denied);
        }
        let mut state = lock(&self.state)?;
        let launch = state.launches.get_mut(token).ok_or(Error::Denied)?;
        if launch.sid != caller.sid
            || launch.image != process.image
            || launch.process.as_ref().is_some_and(|old| old != &process)
            || launch
                .expected_env
                .iter()
                .any(|(key, value)| args.env_var(key) != Some(value.as_str()))
            || launch
                .expected_unset
                .iter()
                .any(|name| args.env_var(name).is_some())
        {
            return Err(Error::Denied);
        }
        if let Some(gate) = &launch.gate {
            gate.current().map_err(|_| Error::Stale)?;
        }
        launch.bind(process.clone());
        Ok(Peer {
            caller: caller.clone(),
            process,
            launch: Some(token.into()),
        })
    }

    pub(super) fn with_initial_source<R>(
        &self,
        env: &Env,
        peer: &Peer,
        connection: &Connection,
        use_initial: impl FnOnce(&owner::AdmittedGrant<'_>, &InitialSourcePermit) -> Result<R, String>,
    ) -> Result<R, String> {
        let home = env.home.as_deref().ok_or("startup profile unavailable")?;
        let token = peer
            .launch
            .as_ref()
            .ok_or("startup requires an owned fresh constructor")?;
        let cell = {
            let state = lock(&self.state).map_err(|_| "startup registry busy")?;
            let launch = state
                .launches
                .get(token)
                .ok_or("startup constructor unavailable")?;
            if launch.process.as_ref() != Some(&peer.process)
                || !peer.current()
                || !connection.is_live()
            {
                return Err("startup constructor changed".into());
            }
            let cell = launch
                .initial
                .as_ref()
                .ok_or("constructor has no native canonical grant")?
                .clone();
            if !cell.alive.load(Ordering::Acquire)
                || cell
                    .installing
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return Err("startup installation busy".into());
            }
            cell
        };
        // No registry mutex is held over grant, book or Slot I/O. The per-launch
        // claim prevents a competing installer and is released on every exit.
        let work = match SourceWorkLease::for_home(home) {
            Ok(work) => work,
            Err(_) => {
                cell.installing.store(false, Ordering::Release);
                return Err("startup source custody busy".into());
            }
        };
        let permit = InitialSourcePermit {
            cell,
            caller: peer.caller.clone(),
            process: peer.process.clone(),
            connection: connection.clone(),
            startup_nonce: token.clone(),
            work,
        };
        permit.current().map_err(|_| "startup permission changed")?;
        owner::with_current_published_grant(env, permit.binding(), &peer.caller, |grant| {
            if grant.canonical_auth() != permit.cell.choice.auth_file() {
                return Err("startup canonical source changed".into());
            }
            // Verify the actual kernel launch pointer, not a raw claimed path.
            let args = atpkg::caller_shell::process_args(peer.caller.pid)
                .ok_or("startup process unreadable")?;
            let home = args.env_var("HOME").map(std::path::Path::new);
            let mut selected = worker::Starter {
                exe: std::path::PathBuf::new(),
                env: Vec::new(),
                args: Ok(Vec::new()),
                unset: Vec::new(),
            };
            for name in ["CODEX_AUTH_PATH", "CODEX_HOME"] {
                if let Some(value) = args.env_var(name) {
                    selected.env.push((name.into(), value.into()));
                }
            }
            let cwd = aterm_sysprobe::process_cwd(peer.caller.pid)
                .ok_or("startup working directory unreadable")?;
            if worker::declared_codex_auth_file(&selected, home, &cwd).as_deref()
                != Some(permit.cell.choice.auth_file())
            {
                return Err("startup launch source changed".into());
            }
            permit.current().map_err(|_| "startup permission changed")?;
            let value = use_initial(&grant, &permit)?;
            permit.current().map_err(|_| "startup permission changed")?;
            Ok(value)
        })
        .and_then(|value| {
            permit
                .current()
                .map_err(|_| "startup permission changed".to_string())?;
            Ok(value)
        })
    }

    /// Existing managed sources only. Unmanaged/startup evidence has a separate
    /// future admission path; a missing Slot never becomes a dummy grant.
    pub fn preload_managed(
        &self,
        env: &Env,
        peer: Peer,
        member: &str,
        operation: &str,
        connection: &Connection,
    ) -> Result<Preload, Error> {
        if !hex(operation, 32) || !connection.alive.load(Ordering::Acquire) || !peer.current() {
            return Err(Error::Denied);
        }
        let key = member_key(&peer.caller, member)?;
        let expected = {
            let state = lock(&self.state)?;
            issued_on(&state, &peer, member, connection)?;
            reservation::reservation_allows_member(&state, &peer.caller, member)?;
            state.members.get(&key).map(|member| member.source.clone())
        };
        let _worker = preload_custody::enter(self, &peer, member, operation, connection)?;
        let gate = SourceWorkLease::for_home(env.home.as_deref().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Busy)?;
        let progress = match expected.as_ref() {
            Some(source) => slot::source_matching_registered(env, &peer.caller, operation, source),
            None => slot::source(env, &peer.caller, operation),
        };
        let source = match progress {
            Ok(slot::SourceProgress::Ready(source)) => Arc::new(source),
            Ok(slot::SourceProgress::Waiting) => return Err(Error::Busy),
            Err(_) if expected.is_some() => return Err(Error::Unavailable),
            Err(_) => {
                let (source, receipt) = self
                    .with_initial_source(env, &peer, connection, |grant, permit| {
                        slot::initialize_startup(env, grant, permit)
                    })
                    .map_err(|_| Error::Unavailable)?;
                // Retain claims through the final peer/owner checks above, then
                // release them before any SDK auth-file load or readiness RPC.
                drop(receipt);
                Arc::new(source)
            }
        };
        let mut state = lock(&self.state)?;
        if let Some(token) = peer.launch.as_ref()
            && let Some(initial) = state
                .launches
                .get(token)
                .and_then(|launch| launch.initial.as_ref())
        {
            initial.published.store(true, Ordering::Release);
            if source.assignment_generation() > 0 {
                initial.alive.store(false, Ordering::Release);
            }
        }
        // Retirement or EOF may race the bounded source read. Never revive a
        // retired native member or publish a source to a disconnected actor.
        issued_on(&state, &peer, member, connection)?;
        reservation::reservation_allows_member(&state, &peer.caller, member)?;
        if state.unrelated.contains_key(&key) {
            return Err(Error::Denied);
        }
        if !connection.alive.load(Ordering::Acquire) || !peer.current() {
            return Err(Error::Stale);
        }
        if let Some(old) = state.members.get(&key) {
            if old.process != peer.process
                || !same_source_scope(&old.source, &source)
                || !revision_descends(
                    source.credential_generation(),
                    source.digest(),
                    old.source.credential_generation(),
                    old.source.digest(),
                )
            {
                return Err(Error::Stale);
            }
            if old.operation == operation && old.connection.same(connection) && !old.unknown {
                return Ok(Preload {
                    member: member.into(),
                    operation: operation.into(),
                    registration: old.registration.clone(),
                    source: old.source.clone(),
                });
            }
        }
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let version = version(&mut state)?;
        let readiness = SourceReadiness::preload(
            gate,
            state.members.get_mut(&key).map(|old| &mut old.readiness),
        )?;
        state.members.insert(
            key,
            Member {
                caller: peer.caller,
                process: peer.process,
                operation: operation.into(),
                registration: registration.clone(),
                connection: connection.clone(),
                version,
                source: source.clone(),
                readiness,
                unknown: false,
            },
        );
        Ok(Preload {
            member: member.into(),
            operation: operation.into(),
            registration,
            source,
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "source correlation, authenticated peer, connection and actual readiness remain explicit"
    )]
    pub fn ready(
        &self,
        env: &Env,
        peer: Peer,
        member: &str,
        operation: &str,
        registration: &str,
        connection: &Connection,
        ready: Ready,
    ) -> Result<(), Error> {
        let key = member_key(&peer.caller, member)?;
        if !peer.current() || !connection.alive.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            issued(&state, &peer, member)?;
            reservation::reservation_allows_member(&state, &peer.caller, member)?;
        }
        let _gate = SourceWorkLease::for_home(env.home.as_deref().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Busy)?;
        let now =
            match slot::source(env, &peer.caller, operation).map_err(|_| Error::Unavailable)? {
                slot::SourceProgress::Ready(source) => source,
                slot::SourceProgress::Waiting => return Err(Error::Busy),
            };
        let mut state = lock(&self.state)?;
        if !connection.is_live() || !peer.current() {
            return Err(Error::Stale);
        }
        if let Some(token) = peer.launch.as_ref() {
            let launch = state.launches.get(token).ok_or(Error::Stale)?;
            if launch.process.as_ref() != Some(&peer.process) {
                return Err(Error::Stale);
            }
        }
        issued(&state, &peer, member)?;
        reservation::reservation_allows_member(&state, &peer.caller, member)?;
        let current = state.members.get_mut(&key).ok_or(Error::Stale)?;
        if current.process != peer.process
            || current.operation != operation
            || current.registration != registration
            || !current.connection.same(connection)
            || current.unknown
            || !current.connection.alive.load(Ordering::Acquire)
            || !same_source_scope(&current.source, &now)
            || current.source.credential_generation() != now.credential_generation()
            || current.source.digest() != now.digest()
        {
            return Err(Error::Stale);
        }
        let role = match ready {
            Ready::Loaded {
                credential_generation,
                digest,
                account,
                user,
                auth,
            } => {
                if credential_generation != current.source.credential_generation()
                    || digest != current.source.digest()
                    || account != current.source.account()
                    || user != current.source.user()
                {
                    return Err(Error::Stale);
                }
                ReadyRole::Manager(auth)
            }
            Ready::ReaderLoaded {
                credential_generation,
                digest,
                account,
                user,
            } => {
                if credential_generation != current.source.credential_generation()
                    || digest != current.source.digest()
                    || account != current.source.account()
                    || user != current.source.user()
                {
                    return Err(Error::Stale);
                }
                ReadyRole::ReaderLoaded
            }
            Ready::PolicyUnavailable => ReadyRole::PolicyUnavailable,
        };
        // A Ready ACK cannot outrun the source worker whose custody it settles.
        preload_custody::check_completion(&state, &key, operation, connection)?;
        let current = state.members.get_mut(&key).ok_or(Error::Stale)?;
        current.readiness.complete(role)?;
        let preload = state.preloads.remove(&key);
        if let Some(token) = peer.launch
            && let Some(launch) = state.launches.get_mut(&token)
        {
            if let Some(initial) = &launch.initial {
                initial.alive.store(false, Ordering::Release);
            }
            launch.gate.take();
            launch._scope.take();
        }
        drop(state);
        drop(preload);
        Ok(())
    }

    pub fn ready_source(
        &self,
        caller: &slot::Caller,
        member: &str,
    ) -> Result<ReadySourceProof, Error> {
        let key = member_key(caller, member)?;
        let state = lock(&self.state)?;
        let current = state.members.get(&key).ok_or(Error::Unavailable)?;
        if !current.readiness.manager()
            || current.unknown
            || !current.connection.alive.load(Ordering::Acquire)
            || Process::observe(caller.pid).as_ref() != Some(&current.process)
        {
            return Err(Error::Stale);
        }
        Ok(ReadySourceProof {
            state: Arc::downgrade(&self.state),
            key,
            version: current.version,
            caller: caller.clone(),
            source: current.source.clone(),
            gate_identity: current.readiness.identity.clone(),
            alive: current.connection.alive.clone(),
        })
    }

    pub fn reader_source(
        &self,
        caller: &slot::Caller,
        member: &str,
    ) -> Result<ReaderSourceProof, Error> {
        let key = member_key(caller, member)?;
        let state = lock(&self.state)?;
        let current = state.members.get(&key).ok_or(Error::Unavailable)?;
        if !current.readiness.reader()
            || current.unknown
            || !current.connection.is_live()
            || Process::observe(caller.pid).as_ref() != Some(&current.process)
        {
            return Err(Error::Stale);
        }
        Ok(ReaderSourceProof {
            state: Arc::downgrade(&self.state),
            key,
            version: current.version,
            caller: caller.clone(),
            source: current.source.clone(),
            gate_identity: current.readiness.identity.clone(),
            alive: current.connection.alive.clone(),
        })
    }

    /// EOF is unknown while the process lives. Only explicit kernel death can
    /// retire the source; a weak manager or task completion is not evidence.
    pub fn connection_closed(&self, connection: u64) {
        let mut connections = connections()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(alive) = connections.remove(&connection) {
            alive.store(false, Ordering::Release);
        }
        drop(connections);
        if let Ok(mut state) = lock(&self.state) {
            for member in state
                .members
                .values_mut()
                .filter(|m| m.connection.id == connection)
            {
                member.unknown = true;
            }
        }
    }

    /// Protect actual live registration connections from the control lane's
    /// idle eviction. Contention is unknown/retained, never a false absence.
    pub fn has_live_connection(&self, connection: u64) -> bool {
        let Ok(state) = lock(&self.state) else {
            return true;
        };
        state.members.values().any(|member| {
            member.connection.id == connection && member.connection.alive.load(Ordering::Acquire)
        }) || state.actors.values().any(|actor| {
            actor.issued.values().any(|issued| {
                issued.connection.id == connection
                    && issued.connection.alive.load(Ordering::Acquire)
            })
        })
    }

    /// Off-main drain step after closing the App admission epoch, before waiting
    /// for its scopes. At most MAX_LAUNCHES permissions are considered; no file
    /// observation or process probing occurs. Busy is retried by the drain worker.
    /// Revocation denies late mandatory-bind consumers; it asserts no process death.
    pub fn revoke_cancelled_unclaimed(&self) -> Result<usize, Error> {
        let mut state = lock(&self.state)?;
        let before = state.launches.len();
        state.launches.retain(|_, launch| {
            !(launch.process.is_none()
                && launch
                    ._scope
                    .as_ref()
                    .is_some_and(|scope| scope.cancelled()))
        });
        Ok(before - state.launches.len())
    }

    pub fn reap_gone(&self) {
        if let Ok(mut state) = lock(&self.state) {
            state.members.retain(|_, member| !gone(&member.process));
            state.unrelated.retain(|_, member| !gone(&member.process));
            state
                .launches
                .retain(|_, launch| !launch.process.as_ref().is_some_and(gone));
            state.actors.retain(|_, actor| !gone(&actor.process));
            let members: std::collections::BTreeSet<String> =
                state.members.keys().cloned().collect();
            state.uses.retain(|key, _| members.contains(key));
            let actors: std::collections::BTreeSet<String> = state.actors.keys().cloned().collect();
            state.reservations.retain(|actor, _| actors.contains(actor));
        }
    }
}

macro_rules! redacted { ($($ty:ty),* $(,)?)=>{$(impl std::fmt::Debug for $ty {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("NativeSourceCustody(redacted)")}
})*}; }
redacted!(
    Peer,
    LaunchReservation,
    Preload,
    ReadySourceProof,
    ReaderSourceProof,
    Registry,
    Connection,
    MemberAllocation,
    InitialSourcePermit
);

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn replaced_gate_reconnect_keeps_old_reader_custody_and_refuses_manager_retarget() {
        let home = aterm_tempfile::tempdir().unwrap();
        let mut reader =
            SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None)
                .unwrap();
        reader.complete(ReadyRole::ReaderLoaded).unwrap();
        let original = home.path().join(".aterm/codex-source-custody/gate");
        std::fs::rename(&original, original.with_extension("old")).unwrap();
        let new = SourceWorkLease::for_home(home.path()).unwrap();
        assert!(SourceReadiness::preload(new, Some(&mut reader)).is_err());
        assert!(reader.gate.is_some());
        let other = aterm_tempfile::tempdir().unwrap();
        let mut manager =
            SourceReadiness::preload(SourceWorkLease::for_home(other.path()).unwrap(), None)
                .unwrap();
        manager.complete(ReadyRole::PolicyUnavailable).unwrap();
        assert!(
            SourceReadiness::preload(
                SourceWorkLease::for_home(home.path()).unwrap(),
                Some(&mut manager)
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_reader_retains_real_gate_through_eof_and_reconnect_until_retirement() {
        // Exercises the exact member readiness/lease transitions with a real
        // kernel gate. It does not fabricate a qualified SDK/source/manager.
        let home = aterm_tempfile::tempdir().unwrap();
        let registry = Registry::default();
        let connection = registry.connection(u64::MAX - 41).unwrap();
        let mut reader =
            SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None)
                .unwrap();
        reader.complete(ReadyRole::ReaderLoaded).unwrap();
        assert!(reader.reader());
        assert!(!reader.manager());
        registry.connection_closed(connection.id);
        assert!(!connection.is_live());
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        let mut reconnected = SourceReadiness::preload(
            SourceWorkLease::for_home(home.path()).unwrap(),
            Some(&mut reader),
        )
        .unwrap();
        drop(reader);
        assert!(!reconnected.reader(), "preload is not a new ready receipt");
        assert!(reconnected.complete(ReadyRole::PolicyUnavailable).is_err());
        assert!(
            reconnected
                .complete(ReadyRole::Manager(AuthState {
                    generation: 1,
                    owner: 2,
                    source: 3,
                    login: 4,
                }))
                .is_err()
        );
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        reconnected.complete(ReadyRole::ReaderLoaded).unwrap();
        reconnected.complete(ReadyRole::ReaderLoaded).unwrap();
        assert!(reconnected.reader());
        assert!(!reconnected.manager());
        drop(reconnected); // same owned-state drop as explicit member retirement
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn reader_proof_role_is_stale_after_gate_path_replacement() {
        let home = aterm_tempfile::tempdir().unwrap();
        let mut reader =
            SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None)
                .unwrap();
        reader.complete(ReadyRole::ReaderLoaded).unwrap();
        let gate = home.path().join(".aterm/codex-source-custody/gate");
        std::fs::rename(&gate, gate.with_extension("displaced")).unwrap();
        let replacement = SourceWorkLease::for_home(home.path()).unwrap();
        assert!(!reader.reader());
        assert_eq!(reader.complete(ReadyRole::ReaderLoaded), Err(Error::Stale));
        assert!(
            reader.gate.is_some(),
            "unknown does not release the old lease"
        );
        drop(replacement);
    }

    #[test]
    fn reader_reconnect_revision_cannot_roll_back_or_replace_equal_generation() {
        assert!(revision_descends(13, "same", 13, "same"));
        assert!(revision_descends(17, "new", 13, "old"));
        assert!(!revision_descends(11, "old", 13, "current"));
        assert!(!revision_descends(13, "other", 13, "current"));
    }

    #[cfg(unix)]
    #[test]
    fn manager_and_policy_readiness_cannot_be_relabelled_as_direct_reader() {
        let home = aterm_tempfile::tempdir().unwrap();
        for role in [
            ReadyRole::PolicyUnavailable,
            ReadyRole::Manager(AuthState {
                generation: 7,
                owner: 11,
                source: 13,
                login: 17,
            }),
        ] {
            let mut readiness =
                SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None)
                    .unwrap();
            readiness.complete(role).unwrap();
            readiness.complete(role).unwrap();
            assert!(!readiness.reader());
            assert!(readiness.complete(ReadyRole::ReaderLoaded).is_err());
            assert!(
                super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn revoking_unclaimed_typed_permission_releases_gate_but_not_claimed_process() {
        // Constructor permission metadata only; this does not mint a qualified
        // SDK peer or a source/Ready proof.
        let home = aterm_tempfile::tempdir().unwrap();
        let process = Process::observe(std::process::id()).unwrap();
        let lifetime = super::super::account_work::Lifetime::default();
        let registry = Registry::default();
        let reserve = |token: &str, claimed: bool| {
            registry.state.lock().unwrap().launches.insert(
                token.into(),
                Launch {
                    sid: "s-revocation-fixture".into(),
                    image: process.image.clone(),
                    expected_env: Vec::new(),
                    expected_unset: Vec::new(),
                    process: claimed.then(|| process.clone()),
                    typed: true,
                    gate: Some(SourceWorkLease::for_home(home.path()).unwrap()),
                    _scope: Some(lifetime.enter().unwrap()),
                    initial: None,
                },
            );
            LaunchReservation {
                token: token.into(),
                executable: std::env::current_exe().unwrap(),
                state: Arc::downgrade(&registry.state),
            }
        };
        let unclaimed = reserve("unclaimed", false);
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        unclaimed.revoke_unclaimed().unwrap();
        let publication =
            super::super::codex_source_gate::PublicationLock::for_home(home.path()).unwrap();
        assert!(
            !registry
                .state
                .lock()
                .unwrap()
                .launches
                .contains_key("unclaimed")
        );
        drop(publication);
        let claimed = reserve("claimed", true);
        assert_eq!(claimed.revoke_unclaimed(), Err(Error::Denied));
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        registry.state.lock().unwrap().launches.clear();
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unrelated_completion_retires_constructor_without_minting_a_source() {
        // Metadata lifecycle only. This deliberately cannot construct a real
        // qualified SDK peer or any positive native source proof.
        let home = aterm_tempfile::tempdir().unwrap();
        let process = Process::observe(std::process::id()).unwrap();
        let caller = slot::Caller {
            sid: "s-unrelated-fixture".into(),
            pid: process.pid,
            birth: process.birth,
        };
        let peer = || Peer {
            caller: caller.clone(),
            process: process.clone(),
            launch: Some("unrelated".into()),
        };
        let registry = Registry::default();
        let connection = registry.connection(u64::MAX - 3).unwrap();
        let lifetime = super::super::account_work::Lifetime::default();
        registry.state.lock().unwrap().launches.insert(
            "unrelated".into(),
            Launch {
                sid: caller.sid.clone(),
                image: process.image.clone(),
                expected_env: Vec::new(),
                expected_unset: Vec::new(),
                process: Some(process.clone()),
                typed: true,
                gate: Some(SourceWorkLease::for_home(home.path()).unwrap()),
                _scope: Some(lifetime.enter().unwrap()),
                initial: None,
            },
        );
        let operation = "1234567890abcdef1234567890abcdef";
        let allocation = registry
            .open_member(&peer(), &connection, operation)
            .unwrap();
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        let registration = registry
            .complete_unrelated(
                peer(),
                &connection,
                &allocation.member,
                operation,
                NoNativeReaderKind::ApiKey,
            )
            .unwrap();
        assert!(registry.ready_source(&caller, &allocation.member).is_err());
        assert!(
            registry.state.lock().unwrap().launches["unrelated"]
                ._scope
                .is_none()
        );
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
        assert_eq!(
            registry
                .complete_unrelated(
                    peer(),
                    &connection,
                    &allocation.member,
                    operation,
                    NoNativeReaderKind::ApiKey
                )
                .unwrap(),
            registration
        );
        assert_eq!(
            registry.complete_unrelated(
                peer(),
                &connection,
                &allocation.member,
                operation,
                NoNativeReaderKind::NoCredentials
            ),
            Err(Error::Stale)
        );
        let replacement = registry.connection(u64::MAX - 4).unwrap();
        assert!(
            registry
                .open_member(&peer(), &replacement, operation)
                .is_err()
        );
        registry.connection_closed(connection.id);
        let reopened = registry
            .open_member(&peer(), &replacement, operation)
            .unwrap();
        assert_eq!(reopened.member, allocation.member);
        let refreshed = registry
            .complete_unrelated(
                peer(),
                &replacement,
                &allocation.member,
                operation,
                NoNativeReaderKind::ApiKey,
            )
            .unwrap();
        assert_ne!(refreshed, registration);
        assert!(
            registry
                .retire_member(
                    peer(),
                    &replacement,
                    &allocation.member,
                    Some(&registration)
                )
                .is_err()
        );
        registry
            .retire_member(peer(), &replacement, &allocation.member, Some(&refreshed))
            .unwrap();
        assert!(
            registry
                .complete_unrelated(
                    peer(),
                    &replacement,
                    &allocation.member,
                    operation,
                    NoNativeReaderKind::ApiKey
                )
                .is_err()
        );
        registry.connection_closed(replacement.id);
    }

    #[cfg(unix)]
    #[test]
    fn app_pause_revokes_only_cancelled_unclaimed_permissions_and_bind_transfers_scope() {
        // Constructor lifetime metadata, not qualified peer/source authority.
        let home = aterm_tempfile::tempdir().unwrap();
        let process = Process::observe(std::process::id()).unwrap();
        let lifetime = super::super::account_work::Lifetime::default();
        let registry = Registry::default();
        let launch = |scope| Launch {
            sid: "s-drain-fixture".into(),
            image: process.image.clone(),
            expected_env: Vec::new(),
            expected_unset: Vec::new(),
            process: None,
            typed: true,
            gate: Some(SourceWorkLease::for_home(home.path()).unwrap()),
            _scope: Some(scope),
            initial: None,
        };
        let pending = lifetime.enter().unwrap();
        let pending_cancel = pending.cancellation();
        registry
            .state
            .lock()
            .unwrap()
            .launches
            .insert("pending".into(), launch(pending));
        let bound = lifetime.enter().unwrap();
        let bound_cancel = bound.cancellation();
        let mut bound = launch(bound);
        bound.bind(process);
        assert!(bound_cancel.retired());
        registry
            .state
            .lock()
            .unwrap()
            .launches
            .insert("bound".into(), bound);
        assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 0);
        assert!(!pending_cancel.retired());
        let _pause = lifetime.pause().unwrap();
        assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 1);
        assert!(pending_cancel.retired());
        assert!(
            !registry
                .state
                .lock()
                .unwrap()
                .launches
                .contains_key("pending")
        );
        assert!(
            registry.state.lock().unwrap().launches["bound"]
                .gate
                .is_some()
        );
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
        assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 0);
        registry.state.lock().unwrap().launches.clear();
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
    }

    #[test]
    fn eof_invalidates_connection_while_member_table_is_busy() {
        let registry = Registry::default();
        let connection = registry.connection(u64::MAX - 1).unwrap();
        let held = registry.state.lock().unwrap();
        registry.connection_closed(connection.id);
        assert!(!connection.alive.load(Ordering::Acquire));
        drop(held);
    }

    #[test]
    fn orderly_allocation_retirement_is_bounded_and_never_reuses_member_ids() {
        // Allocation metadata only: no native source/credential or Ready proof
        // is constructed. Production Peer constructors require the qualified
        // image plus the actual PTY/constructor; this uses our real kernel PID.
        let process = Process::observe(std::process::id()).unwrap();
        let caller = slot::Caller {
            sid: "s-allocation-fixture".into(),
            pid: process.pid,
            birth: process.birth,
        };
        let peer = || Peer {
            caller: caller.clone(),
            process: process.clone(),
            launch: None,
        };
        let registry = Registry::default();
        let connection = registry.connection(u64::MAX - 2).unwrap();
        let mut first = None;
        for number in 0..(MAX_MEMBERS + 32) {
            let operation = format!("{number:032x}");
            let allocation = registry
                .open_member(&peer(), &connection, &operation)
                .unwrap();
            assert!(hex(&allocation.member, 32));
            if first.is_none() {
                first = Some(allocation.member.clone());
            }
            registry
                .retire_member(peer(), &connection, &allocation.member, None)
                .unwrap();
            assert!(issued(&registry.state.lock().unwrap(), &peer(), &allocation.member).is_err());
        }
        let state = registry.state.lock().unwrap();
        let actor = state.actors.get(&actor_key(&caller)).unwrap();
        assert!(actor.issued.is_empty());
        assert_eq!(actor.highwater, (MAX_MEMBERS + 32) as u64);
        assert!(!actor.issued.contains_key(&first.unwrap()));
        drop(state);
        registry.connection_closed(connection.id);
    }
}
