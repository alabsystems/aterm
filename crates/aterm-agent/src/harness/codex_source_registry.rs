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

#[cfg(unix)]
#[path = "codex_source_owned_launch.rs"]
mod owned_launch;
#[cfg(unix)]
pub use owned_launch::{OwnedLaunchReservation, OwnedSpawnFailure, OwnedWorker};
#[cfg(unix)]
#[path = "codex_source_owned_environment.rs"]
mod owned_environment;
#[cfg(unix)]
pub use owned_environment::OwnedLaunchEnvironment;

#[path = "codex_source_restart.rs"]
mod restart;
pub use restart::RestartReservation;
pub use restart::SourceExecReservation;
#[cfg(all(unix, any(test, debug_assertions)))]
#[path = "codex_source_proof_observation.rs"]
mod proof_observation;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use proof_observation::{SOURCE_PROOF_OBSERVATION_LIMIT, SourceProofObservation};

#[path = "codex_source_derived.rs"]
mod derived;
#[path = "codex_source_plain_retirement.rs"]
mod plain_retirement;
#[path = "codex_source_preload_custody.rs"]
mod preload_custody;
#[path = "codex_source_preload_sequence.rs"]
mod preload_sequence;
pub use preload_sequence::{PreloadPredecessor, PreloadSequence};
#[path = "codex_source_reservation.rs"]
mod reservation;
pub use reservation::namespace::{
    CapturedPreloadCommand, CapturedReadyCommand, CapturedReadyPreparation,
    OwnedPreloadPreparation, OwnedUseRefresh, PreloadResample, PreloadUseIdentity,
    ReboundReadyCommand, ReboundReadyOutcome, ReboundReadyRequest, ResamplePreparation,
};
pub(in crate::harness) use reservation::namespace::{PreloadReadNamespace, UseNamespace};
pub(in crate::harness) use reservation::target::TargetSnapshotRead;
pub use reservation::target::normalize::NormalizedMemberRequest;
pub(in crate::harness) use reservation::target::normalize::NormalizedRead;
pub(in crate::harness) use reservation::target::resources::TargetAckPermit;
pub use reservation::target::{
    AckPreparation, MemberRetirementPreparation, MemberRetirementRequest, OwnedMemberRetirement,
    ResourcePreparation, ResourceReport, TargetAckCommand, TargetApplied, TargetNoReaderCommand,
    TargetNoReaderPreparation, TargetNoReaderRequest, TargetOpenPreparation, TargetOpenRetirement,
    TargetPreloadPreparation, TargetReadyCommand, TargetReadyPreparation, TargetReadyReport,
    TargetResampleCommand, TargetResamplePreparation, TargetResampleRequest, TargetResourceCommand,
    TargetResourceReceipt, WorkRebindKind, WorkRebindRequest,
};
pub use reservation::{
    BeginAdmission, CensusCondition, CensusCursor, CensusMember, CensusPage, CleanupAdmission,
    InstalledTarget, OwnedUseCleanup, Quiescence, RegisteredMemberCleanup, RegisteredMemberWork,
    Reservation, SourceSeal, SourceUse, TargetConstruction, TargetOpenCleanup, TargetReady,
    UseAction, UseCleanupRequest, UseCommand, UseIdentity, UseNonce, UseProgress, UseReceipt,
    UseRequest,
};
pub(in crate::harness) use reservation::{
    seal::SealingSource,
    target::{InstalledRead, TargetAdmission},
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

enum LaunchBinding {
    Consumer,
    #[cfg(unix)]
    Native(Arc<owned_launch::Binding>),
}
impl LaunchBinding {
    fn entry(&self) -> Result<(), Error> {
        match self {
            Self::Consumer => Ok(()),
            #[cfg(unix)]
            Self::Native(binding) => binding.entry(),
        }
    }
    fn revoked(&self) -> bool {
        match self {
            Self::Consumer => false,
            #[cfg(unix)]
            Self::Native(binding) => binding.revoked(),
        }
    }
}

struct Launch {
    sid: String,
    image: Image,
    expected_env: Vec<(String, String)>,
    expected_unset: Vec<&'static str>,
    process: Option<Process>,
    binding: LaunchBinding,
    typed: bool,
    gate: Option<Arc<SourceWorkLease>>,
    _scope: Option<super::account_work::Scope>,
    initial: Option<Arc<InitialCell>>,
    native_restart: Option<Arc<restart::NativeRestart>>,
}

impl Drop for Launch {
    fn drop(&mut self) {
        if let Some(initial) = &self.initial {
            initial.alive.store(false, Ordering::Release);
        }
    }
}

impl Launch {
    fn bind(&mut self, process: Process) -> Result<(), Error> {
        if let Some(native) = &self.native_restart {
            native.bind_generation(&process)?;
        }
        self.process = Some(process);
        self.typed = true;
        // Native constructor work is complete at actual kernel bind. Consumer
        // and connection lifetime now owns startup; the gate is NOT released.
        // This scope never contains an irreversible provider response.
        self._scope.take();
        Ok(())
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
    /// Borrow the real constructor gate for a counted natural quota read.
    /// Kernel custody alone does not authorize any credential or provider use.
    pub fn native_usage_source(
        &self,
        scope: &super::account_work::Scope,
    ) -> Result<owner::NativeUsageSourceLease, Error> {
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let capture = || {
            let state = lock(&state)?;
            let launch = state.launches.get(&self.token).ok_or(Error::Stale)?;
            if launch.typed
                || launch.process.is_some()
                || scope.cancelled()
                || !launch
                    ._scope
                    .as_ref()
                    .is_some_and(|held| held.operation_identity() == scope.operation_identity())
            {
                return Err(Error::Stale);
            }
            Ok((
                launch.gate.as_ref().ok_or(Error::Stale)?.clone(),
                launch.image.clone(),
            ))
        };
        let (gate, image) = capture()?;
        // No filesystem/kernel observation occurs under the registry mutex.
        gate.current().map_err(|_| Error::Stale)?;
        if scope.cancelled() || Image::at(&self.executable).as_ref() != Some(&image) {
            return Err(Error::Stale);
        }
        let (current, current_image) = capture()?;
        if !Arc::ptr_eq(&gate, &current) || image != current_image {
            return Err(Error::Stale);
        }
        Ok(owner::NativeUsageSourceLease { inner: gate })
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
        self.revoke_unclaimed_inner()
    }
    fn revoke_unclaimed_inner(&self) -> Result<(), Error> {
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

enum ReaderGate {
    Global(Arc<SourceWorkLease>),
    Target(Arc<super::codex_source_gate::TargetFamilyLease>),
}
impl ReaderGate {
    fn current(&self) -> Result<(), &'static str> {
        match self {
            Self::Global(gate) => gate.current(),
            Self::Target(gate) => gate.current(),
        }
    }
}
struct SourceReadiness {
    gate: Option<ReaderGate>,
    identity: super::codex_source_gate::SourceGateIdentity,
    role: Option<ReadyRole>,
    // Reconnect does not retire any earlier direct-reader/derived use.
    reader_owned: bool,
}
impl SourceReadiness {
    fn preload(gate: SourceWorkLease, previous: Option<&mut Self>) -> Result<Self, Error> {
        Self::preload_shared(Arc::new(gate), previous)
    }
    fn preload_shared(
        gate: Arc<SourceWorkLease>,
        previous: Option<&mut Self>,
    ) -> Result<Self, Error> {
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
        let gate = previous_reader.unwrap_or(ReaderGate::Global(gate));
        // All fallible checks precede transferring an old reader's held lock.
        Ok(Self {
            gate: Some(gate),
            identity,
            role: None,
            reader_owned,
        })
    }
    fn target(
        resource: Arc<super::codex_source_gate::TargetFamilyLease>,
        reader_owned: bool,
    ) -> Result<Self, Error> {
        resource.current().map_err(|_| Error::Stale)?;
        Ok(Self {
            identity: resource.gate_identity().clone(),
            gate: Some(ReaderGate::Target(resource)),
            role: None,
            reader_owned,
        })
    }
    fn complete(&mut self, role: ReadyRole) -> Result<(), Error> {
        if let Some(gate) = &self.gate {
            gate.current().map_err(|_| Error::Stale)?;
        }
        self.complete_recorded(role)
    }
    fn complete_recorded(&mut self, role: ReadyRole) -> Result<(), Error> {
        if (self.reader_owned && role != ReadyRole::ReaderLoaded)
            || self.role.is_some_and(|previous| previous != role)
            || (role == ReadyRole::ReaderLoaded && self.gate.is_none())
        {
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
    work_rebound: bool,
}

struct Issued {
    operation: String,
    connection: Connection,
    parent_use: Option<reservation::ParentUse>,
    target: Option<Weak<reservation::target::Installed>>,
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
    // Only in-flight release custody; settled closure is Actor ordinal metadata.
    plain_retiring: BTreeMap<String, ()>,
    launches: BTreeMap<String, Launch>,
    actors: BTreeMap<String, Actor>,
    uses: BTreeMap<String, reservation::Uses>,
    preloads: BTreeMap<String, Arc<preload_custody::Pending>>,
    preload_sequences: BTreeMap<String, preload_sequence::Ledger>,
    reservations: BTreeMap<String, Arc<Mutex<reservation::Cell>>>,
    retired_reservations: BTreeMap<String, Arc<Mutex<reservation::Cell>>>,
    target_opens: BTreeMap<String, reservation::target::OpenRecord>,
    target_resources: BTreeMap<String, reservation::target::resources::Record>,
    normalized_targets: BTreeMap<String, Arc<reservation::target::normalize::NormalizedTarget>>,
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

/// Fixtures share the process-wide connection table, so their IDs must share
/// one allocator too. Keep them outside the small IDs used by wire fixtures.
#[cfg(test)]
fn fixture_connection_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX / 2);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("fixture connection IDs exhausted")
}

/// Own the fixture's front door independently of any cloned work tokens.
/// Dropping the front door delivers real EOF and releases its bounded slot.
#[cfg(test)]
pub(in crate::harness) struct FixtureConnection {
    registry: Registry,
    connection: Connection,
}
#[cfg(test)]
impl std::ops::Deref for FixtureConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.connection
    }
}
#[cfg(test)]
impl Drop for FixtureConnection {
    fn drop(&mut self) {
        self.registry.connection_closed(self.connection.id);
    }
}
#[cfg(test)]
impl Registry {
    pub(in crate::harness) fn fixture_connection(&self) -> FixtureConnection {
        FixtureConnection {
            registry: Registry {
                state: self.state.clone(),
            },
            connection: self.connection(fixture_connection_id()).unwrap(),
        }
    }
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

#[derive(Clone)]
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
        // Observe the kernel outside State, then compare the complete process
        // identity under the same metadata guard as the retained version.
        let observed = Process::observe(self.caller.pid);
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        let Ok(state) = lock(&state) else {
            return false;
        };
        let current = !reservation::target::normalize::handoff::source_is_revoked(
            &state,
            &self.caller,
            &self.source,
        ) && state.members.get(&self.key).is_some_and(|member| {
            member.version == self.version
                && member.readiness.manager()
                && !member.unknown
                && observed.as_ref() == Some(&member.process)
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
        let observed = Process::observe(self.caller.pid);
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        let Ok(state) = lock(&state) else {
            return false;
        };
        let current = !reservation::target::normalize::handoff::source_is_revoked(
            &state,
            &self.caller,
            &self.source,
        ) && state.members.get(&self.key).is_some_and(|member| {
            member.version == self.version
                && member.readiness.reader()
                && !member.unknown
                && observed.as_ref() == Some(&member.process)
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
        restart::advance_exec_actor(&mut state, peer)?;
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
            if issued.target.is_some() || issued.parent_use != parent_use {
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
                target: None,
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
        if state.preload_sequences.contains_key(&key) {
            // Owned PRELOAD has exact release-only retirement; the legacy
            // member verb cannot discard its fence or manufacture a receipt.
            return Err(Error::Denied);
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
        reservation::target::resources::check_retirement(&state, &peer.caller, member)?;
        let target_constructor =
            reservation::target::retire_constructor(&state, &peer.caller, member)?;
        let target_resources =
            reservation::target::resources::revoke_member(&mut state, &peer.caller, member);
        let preload = state.preloads.remove(&key);
        // Exact settled use receipts remain metadata for release-only replay.
        // No Scope survives explicit member retirement.
        state.members.remove(&key);
        state.unrelated.remove(&key);
        state
            .actors
            .get_mut(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?
            .issued
            .remove(member);
        version(&mut state)?;
        let normalized = reservation::target::normalize::take_unused(&mut state);
        drop(state);
        drop(normalized);
        drop(preload);
        drop(target_constructor);
        drop(target_resources);
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
        if allocation.target.is_some() {
            return Err(Error::Denied);
        }
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
        reservation::target::resources::check_member_update(&state, &peer.caller, member)?;
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
        self.reserve_launch(env, sid, admitted, scope, LaunchBinding::Consumer)
    }

    /// Reserve an independently admitted constructor for a native-owned child.
    /// The consumer cannot claim it until its launcher binds actual child custody.
    /// This does not inherit a parent's SDK member or authorize provider use.
    ///
    /// # Errors
    /// As [`Self::reserve_worker`]; the native child must additionally be bound
    /// through [`OwnedLaunchReservation::bind`] before it can open a member.
    /// An unreadable/changed source environment or an incomplete admission view
    /// also refuses. The selected environment is sealed through native spawn.
    #[cfg(unix)]
    pub fn reserve_owned_worker(
        &self,
        env: &Env,
        sid: &str,
        admitted: &worker::Admitted,
        scope: &super::account_work::Scope,
        environment: OwnedLaunchEnvironment,
    ) -> Result<OwnedLaunchReservation, Error> {
        let environment = environment.admit(admitted.starter())?;
        let binding = Arc::new(owned_launch::Binding::default());
        self.reserve_launch(
            env,
            sid,
            admitted,
            scope,
            LaunchBinding::Native(binding.clone()),
        )
        .map(|inner| OwnedLaunchReservation::new(inner, binding, environment))
    }

    fn reserve_launch(
        &self,
        env: &Env,
        sid: &str,
        admitted: &worker::Admitted,
        scope: &super::account_work::Scope,
        binding: LaunchBinding,
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
                binding,
                typed: false,
                gate: Some(Arc::new(gate)),
                _scope: Some(scope.clone()),
                initial,
                native_restart: None,
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
            || launch
                .native_restart
                .as_ref()
                .is_some_and(|native| !native.admits(&process))
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
        // An owned child retries its existing constructor operation; possession
        // of the launch environment cannot substitute for native spawn custody.
        launch.binding.entry()?;
        if let Some(gate) = &launch.gate {
            gate.current().map_err(|_| Error::Stale)?;
        }
        launch.bind(process.clone())?;
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
        sequence: PreloadSequence,
    ) -> Result<Preload, Error> {
        if !hex(operation, 32) || !connection.alive.load(Ordering::Acquire) || !peer.current() {
            return Err(Error::Denied);
        }
        let key = member_key(&peer.caller, member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Ordinary,
        };
        let expected = {
            let state = lock(&self.state)?;
            issued_on(&state, &peer, member, connection)?;
            if state.unrelated.contains_key(&key) {
                return Err(Error::Denied);
            }
            reservation::target::resources::check_member_update(&state, &peer.caller, member)?;
            if state
                .actors
                .get(&actor_key(&peer.caller))
                .and_then(|actor| actor.issued.get(member))
                .is_some_and(|issued| issued.target.is_some())
            {
                return Err(Error::Denied);
            }
            reservation::reservation_allows_member(&state, &peer.caller, member)?;
            if let Some(ledger) = state.preload_sequences.get(&key) {
                ledger.check_attempt(&peer.process, &attempt)?;
                if let Some(original) = ledger.initial_preload(&attempt) {
                    return Ok(original);
                }
            }
            state.members.get(&key).map(|member| member.source.clone())
        };
        let _worker =
            preload_custody::enter_sequenced(self, &peer, member, operation, connection, &attempt)?;
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
        preload_sequence::check_current(&state, &key, &peer.process, &attempt)?;
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
        reservation::target::resources::check_member_update(&state, &peer.caller, member)?;
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let version = version(&mut state)?;
        let readiness = SourceReadiness::preload(
            gate,
            state.members.get_mut(&key).map(|old| &mut old.readiness),
        )?;
        state
            .preload_sequences
            .get_mut(&key)
            .ok_or(Error::Stale)?
            .loaded(
                &attempt,
                &registration,
                source.credential_generation(),
                source.digest(),
            )?;
        state
            .preload_sequences
            .get_mut(&key)
            .ok_or(Error::Stale)?
            .record_initial(
                &attempt,
                Preload {
                    member: member.into(),
                    operation: operation.into(),
                    registration: registration.clone(),
                    source: source.clone(),
                },
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
                work_rebound: false,
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
        sequence: PreloadSequence,
    ) -> Result<(), Error> {
        let key = member_key(&peer.caller, member)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Ordinary,
        };
        if !peer.current() || !connection.alive.load(Ordering::Acquire) {
            return Err(Error::Stale);
        }
        let recorded = || -> Result<bool, Error> {
            let state = lock(&self.state)?;
            preload_sequence::check_current(&state, &key, &peer.process, &attempt)?;
            issued_on(&state, &peer, member, connection)?;
            reservation::reservation_allows_member(&state, &peer.caller, member)?;
            let current = state.members.get(&key).ok_or(Error::Stale)?;
            if current.process != peer.process
                || current.operation != operation
                || current.registration != registration
                || !current.connection.same(connection)
            {
                return Err(Error::Stale);
            }
            let role = reservation::target::reported_role(&ready, &current.source)?;
            // Reconcile an exact committed report after a lost ACK even while
            // the authenticated rebound member is unknown. This returns only
            // metadata: it does not clear unknown or restore native Ready.
            Ok(!state.preloads.contains_key(&key)
                && current.readiness.role == Some(role)
                && state
                    .preload_sequences
                    .get(&key)
                    .is_some_and(|ledger| ledger.recorded_ready(&attempt, registration, role)))
        };
        if recorded()? {
            return Ok(());
        }
        let expected = {
            let state = lock(&self.state)?;
            preload_sequence::check_current(&state, &key, &peer.process, &attempt)?;
            issued(&state, &peer, member)?;
            if state
                .actors
                .get(&actor_key(&peer.caller))
                .and_then(|actor| actor.issued.get(member))
                .is_some_and(|issued| issued.target.is_some())
            {
                return Err(Error::Denied);
            }
            reservation::reservation_allows_member(&state, &peer.caller, member)?;
            state.members.get(&key).ok_or(Error::Stale)?.source.clone()
        };
        let worker =
            match preload_custody::enter_completion(self, &peer, member, operation, connection) {
                Ok(worker) => worker,
                Err(error) => {
                    if recorded()? {
                        return Ok(());
                    }
                    return Err(error);
                }
            };
        let _gate = SourceWorkLease::for_home(env.home.as_deref().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Busy)?;
        let now = match slot::source_matching_registered(env, &peer.caller, operation, &expected)
            .map_err(|_| Error::Unavailable)?
        {
            slot::SourceProgress::Ready(source) => source,
            slot::SourceProgress::Waiting => return Err(Error::Busy),
        };
        let mut state = lock(&self.state)?;
        preload_sequence::check_current(&state, &key, &peer.process, &attempt)?;
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
        worker.check_completion(&state, &key)?;
        reservation::target::resources::check_member_update(&state, &peer.caller, member)?;
        // Keep the accepted immutable report with the completed sequence, as
        // the owned PRELOAD path does. A completed bit alone cannot prove which
        // Ready body was accepted or support coherent native observation.
        let mut ledger = state
            .preload_sequences
            .get(&key)
            .ok_or(Error::Stale)?
            .clone();
        ledger.accept_ready(&attempt, registration, role)?;
        ledger.complete(&attempt, registration)?;
        let current = state.members.get_mut(&key).ok_or(Error::Stale)?;
        current.readiness.complete(role)?;
        state.preload_sequences.insert(key.clone(), ledger);
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
        let observed = Process::observe(caller.pid);
        let state = lock(&self.state)?;
        let current = state.members.get(&key).ok_or(Error::Unavailable)?;
        if reservation::target::normalize::handoff::source_revoked(&state, caller, member)
            || !current.readiness.manager()
            || current.unknown
            || !current.connection.alive.load(Ordering::Acquire)
            || observed.as_ref() != Some(&current.process)
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
        let observed = Process::observe(caller.pid);
        let state = lock(&self.state)?;
        let current = state.members.get(&key).ok_or(Error::Unavailable)?;
        if reservation::target::normalize::handoff::source_revoked(&state, caller, member)
            || !current.readiness.reader()
            || current.unknown
            || !current.connection.is_live()
            || observed.as_ref() != Some(&current.process)
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
    /// for its scopes. Bounded launch/resource permissions are considered; no file
    /// observation or process probing occurs. Busy is retried by the drain worker.
    /// Revocation denies late mandatory-bind consumers; it asserts no process death.
    pub fn revoke_cancelled_unclaimed(&self) -> Result<usize, Error> {
        let mut state = lock(&self.state)?;
        let before = state.launches.len();
        state.launches.retain(|_, launch| {
            !(launch.binding.revoked()
                || (launch.process.is_none()
                    && launch
                        ._scope
                        .as_ref()
                        .is_some_and(|scope| scope.cancelled())))
        });
        let retired = reservation::target::resources::refuse_cancelled(&mut state);
        let removed = before - state.launches.len() + retired.len();
        drop(state);
        drop(retired);
        Ok(removed)
    }

    pub fn reap_gone(&self) {
        let (mut processes, target_owners) = {
            let Ok(state) = lock(&self.state) else { return };
            let mut owners = reservation::target::owners(&state);
            owners.extend(reservation::target::resources::owners(&state));
            let processes = state
                .members
                .values()
                .map(|item| item.process.clone())
                .chain(state.unrelated.values().map(|item| item.process.clone()))
                .chain(state.actors.values().map(|item| item.process.clone()))
                .chain(state.launches.values().flat_map(|item| {
                    item.process.clone().into_iter().chain(
                        item.native_restart
                            .as_ref()
                            .map(|native| native.child.clone()),
                    )
                }))
                .chain(owners.iter().map(|(_, process)| process.clone()))
                .collect::<Vec<_>>();
            (processes, owners)
        };
        // Deduplicate outside State: many accepted operations share one caller.
        processes.sort_by(|a, b| {
            (
                a.pid,
                a.birth,
                a.parent,
                a.group,
                a.session,
                a.image.device,
                a.image.inode,
                &a.image.path,
            )
                .cmp(&(
                    b.pid,
                    b.birth,
                    b.parent,
                    b.group,
                    b.session,
                    b.image.device,
                    b.image.inode,
                    &b.image.path,
                ))
        });
        processes.dedup();
        // Process observation stays outside State. Exact process identities are
        // rechecked below; an unread process is never treated as departed.
        let departed = processes.into_iter().filter(gone).collect::<Vec<_>>();
        let is_gone = |process: &Process| departed.iter().any(|old| old == process);
        let mut target_owners = target_owners
            .into_iter()
            .filter(|(_, process)| is_gone(process))
            .collect::<Vec<_>>();
        target_owners.sort_by(|(a, ap), (b, bp)| {
            (
                actor_key(a),
                ap.image.device,
                ap.image.inode,
                &ap.image.path,
            )
                .cmp(&(
                    actor_key(b),
                    bp.image.device,
                    bp.image.inode,
                    &bp.image.path,
                ))
        });
        target_owners.dedup_by(|a, b| a == b);

        let departed_preloads = {
            let Ok(state) = lock(&self.state) else { return };
            state
                .actors
                .iter()
                .filter(|(_, actor)| is_gone(&actor.process))
                .flat_map(|(actor, _)| {
                    let prefix = format!("{actor}:");
                    state
                        .preloads
                        .iter()
                        .filter(move |(key, _)| key.starts_with(&prefix))
                        .map(|(key, pending)| (key.clone(), pending.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (_, pending) in &departed_preloads {
            pending.revoke_departed();
        }
        let Ok(mut state) = lock(&self.state) else {
            return;
        };
        let mut target_constructors = Vec::new();
        let mut target_resources = Vec::new();
        for (caller, process) in &target_owners {
            if let Ok(retired) =
                reservation::target::revoke_dead_constructor_custody(&state, caller, process)
            {
                target_constructors.extend(retired);
            }
            target_resources.push(reservation::target::resources::revoke_departed(
                &mut state, caller, process,
            ));
        }
        let preloads = departed_preloads
            .iter()
            .filter(|(key, pending)| {
                pending.retired()
                    && state
                        .preloads
                        .get(key)
                        .is_some_and(|actual| Arc::ptr_eq(actual, pending))
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|key| state.preloads.remove(&key))
            .collect::<Vec<_>>();
        let keys = state
            .members
            .iter()
            .filter(|(_, item)| is_gone(&item.process))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let scopes = reservation::retire_gone_uses(&mut state, &keys);
        let members = keys
            .iter()
            .filter_map(|key| state.members.remove(key))
            .collect::<Vec<_>>();
        let unrelated_keys = state
            .unrelated
            .iter()
            .filter(|(_, item)| is_gone(&item.process))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let unrelated = unrelated_keys
            .iter()
            .filter_map(|key| state.unrelated.remove(key))
            .collect::<Vec<_>>();
        let launch_keys = state
            .launches
            .iter()
            .filter(|(_, item)| {
                item.binding.revoked()
                    || item.process.as_ref().is_some_and(is_gone)
                    || item
                        .native_restart
                        .as_ref()
                        .is_some_and(|native| native.cancelled() || is_gone(&native.child))
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let launches = launch_keys
            .iter()
            .filter_map(|key| state.launches.remove(key))
            .collect::<Vec<_>>();
        let mut actor_keys = state
            .actors
            .iter()
            .filter(|(key, actor)| {
                is_gone(&actor.process)
                    && !target_owners.iter().any(|(caller, process)| {
                        actor_key(caller) == **key
                            && *process == actor.process
                            && (reservation::target::constructor_custody_pending(&state, caller)
                                || reservation::target::resources::custody_pending(&state, caller))
                    })
                    && !state
                        .preloads
                        .keys()
                        .any(|member| member.starts_with(&format!("{key}:")))
                    && !state.uses.iter().any(|(member, _)| {
                        member.starts_with(&format!("{key}:"))
                            && reservation::has_credential_uses(&state, member)
                    })
                    && !state.preload_sequences.iter().any(|(member, ledger)| {
                        member.starts_with(&format!("{key}:"))
                            && ledger.fenced()
                            && !ledger.retired()
                    })
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut departed_custody = Vec::new();
        actor_keys.retain(|key| {
            let owner = target_owners.iter().find(|(caller, process)| {
                actor_key(caller) == *key
                    && state
                        .actors
                        .get(key)
                        .is_some_and(|actor| actor.process == *process)
            });
            if let Some((caller, process)) = owner {
                match reservation::target::take_departed_custody(&state, caller, process) {
                    Ok(Some(custody)) => {
                        departed_custody.push(custody);
                        true
                    }
                    Ok(None) => true,
                    Err(_) => false,
                }
            } else {
                !state.reservations.contains_key(key)
            }
        });
        let departed_open_owners = actor_keys
            .iter()
            .filter_map(|key| {
                state
                    .actors
                    .get(key)
                    .map(|actor| (key.clone(), actor.process.clone()))
            })
            .collect::<Vec<_>>();
        let open_custody =
            reservation::target::take_departed_open_custody(&mut state, &departed_open_owners);
        let actors = actor_keys
            .iter()
            .filter_map(|key| state.actors.remove(key))
            .collect::<Vec<_>>();
        let reservations = actor_keys
            .iter()
            .filter_map(|key| state.reservations.remove(key))
            .collect::<Vec<_>>();
        let retired_keys = state
            .retired_reservations
            .keys()
            .filter(|key| {
                actor_keys
                    .iter()
                    .any(|actor| key.starts_with(&format!("{actor}:")))
            })
            .cloned()
            .collect::<Vec<_>>();
        let retired_reservations = retired_keys
            .into_iter()
            .filter_map(|key| state.retired_reservations.remove(&key))
            .collect::<Vec<_>>();
        let stale_uses = state
            .uses
            .keys()
            .filter(|key| {
                (actor_keys
                    .iter()
                    .any(|actor| key.starts_with(&format!("{actor}:")))
                    || state.actors.iter().any(|(actor, current)| {
                        is_gone(&current.process) && key.starts_with(&format!("{actor}:"))
                    }))
                    && !reservation::has_credential_uses(&state, key)
            })
            .cloned()
            .collect::<Vec<_>>();
        let uses = stale_uses
            .iter()
            .filter_map(|key| state.uses.remove(key))
            .collect::<Vec<_>>();
        // Exact terminal PRELOAD receipts have no Scope. Keep them for the
        // live native incarnation; only actual kernel departure discards them.
        let sequence_keys = state
            .preload_sequences
            .keys()
            .filter(|key| {
                actor_keys
                    .iter()
                    .any(|actor| key.starts_with(&format!("{actor}:")))
            })
            .cloned()
            .collect::<Vec<_>>();
        let sequences = sequence_keys
            .into_iter()
            .filter_map(|key| state.preload_sequences.remove(&key))
            .collect::<Vec<_>>();
        let normalizations = reservation::target::normalize::take_unused(&mut state);
        drop(state);
        drop((
            departed_custody,
            target_constructors,
            target_resources,
            normalizations,
            members,
            unrelated,
            launches,
            actors,
            reservations,
            retired_reservations,
            uses,
            sequences,
            open_custody,
            scopes,
            preloads,
            departed_preloads,
        ));
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
#[path = "codex_source_registry_tests.rs"]
mod tests;
