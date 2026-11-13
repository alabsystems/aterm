// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Installed-target authority is issued only after fresh canonical admission,
//! actual signed Book installation, and the real Slot materialization receipt.
use super::*;
#[path = "codex_source_target_normalize.rs"]
pub(in crate::harness) mod normalize;
use crate::harness::{
    codex_assignment as assignment,
    codex_source_gate::{ReservationFamilyExclusive, TargetFamilyLease},
};
#[path = "codex_source_member_retirement.rs"]
mod member_retirement;
#[path = "codex_source_target_ready.rs"]
mod ready;
pub use member_retirement::{
    MemberRetirementPreparation, MemberRetirementRequest, OwnedMemberRetirement,
};
pub use ready::{TargetReadyCommand, TargetReadyPreparation, TargetReadyReport};
#[path = "codex_source_target_resources.rs"]
pub(in crate::harness) mod resources;
pub(in crate::harness) use resources::TargetSnapshotRead;
pub use resources::{
    AckPreparation, ResourcePreparation, ResourceReport, TargetAckCommand, TargetApplied,
    TargetResourceCommand, TargetResourceReceipt,
};

pub(in crate::harness) struct Installed {
    context: String,
    source: Arc<slot::SourceHandle>,
    assignment: Arc<assignment::AssignmentCommit>,
    resource: Arc<TargetFamilyLease>,
    boundary: String,
}
impl Installed {
    pub(super) fn source(&self) -> &Arc<slot::SourceHandle> {
        &self.source
    }
    pub(super) fn assignment(&self) -> &Arc<assignment::AssignmentCommit> {
        &self.assignment
    }
    pub(super) fn resource(&self) -> &Arc<TargetFamilyLease> {
        &self.resource
    }
}

/// Acceptance evidence survives context expiry and transport reply eviction.
/// Active/unknown attempts are never removed to make room for another OPEN.
pub(in crate::harness) struct OpenRecord {
    caller: slot::Caller,
    process: Process,
    context: String,
    assignment: String,
    member: String,
    parent_use: Option<ParentUse>,
    validated: bool,
    preloads: std::collections::BTreeSet<String>,
    latest_preload: Option<String>,
    cleanup: Option<OpenClosure>,
    closed: Option<OpenClosure>,
    member_retirement: Option<preload_sequence::Retirement>,
    member_retired: bool,
    no_reader: Option<no_reader::Completion>,
    cleanup_turn: Option<Arc<crate::harness::account_work::ReleaseTurn>>,
}
impl OpenRecord {
    fn admits_preload(&self, member: &str, operation: &str) -> Result<(), Error> {
        if self.closed.is_some()
            || self.member_retired
            || self.cleanup.is_some()
            || self.member_retirement.is_some()
            || self.member != member
            || (self.preloads.contains(operation)
                && self.latest_preload.as_deref() != Some(operation))
        {
            return Err(Error::Stale);
        }
        if !self.preloads.contains(operation) && self.preloads.len() >= MAX_MEMBERS {
            return Err(Error::Busy);
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq)]
struct OpenClosure {
    operation: String,
    member: Option<String>,
    context: Option<(String, String)>,
}
fn open_key(caller: &slot::Caller, operation: &str) -> String {
    format!("{}:{operation}", actor_key(caller))
}

/// Exact release-only action. It has no conversion to a Peer, installed role,
/// source permission, or ordinary work Scope.
pub struct TargetOpenRetirement<'a> {
    pub operation: &'a str,
    pub open_operation: &'a str,
    pub assignment: &'a str,
    pub context: &'a str,
    pub member: Option<&'a str>,
}

pub struct TargetOpenCleanup {
    state: Weak<Mutex<State>>,
    caller: slot::Caller,
    process: Process,
    connection: Connection,
    open: String,
    request: OpenClosure,
    _scope: Option<crate::harness::account_work::Scope>,
    _turn: Option<Arc<crate::harness::account_work::ReleaseTurn>>,
}
impl std::fmt::Debug for TargetOpenCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetOpenCleanup(redacted)")
    }
}
/// Non-deserializable live role. An operation ID, SourceSeal or signed Book
/// alone cannot construct this or authorize a B factory/read.
pub struct InstalledTarget {
    state: Weak<Mutex<State>>,
    cell: Weak<Mutex<Cell>>,
    installed: Weak<Installed>,
}
impl std::fmt::Debug for InstalledTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InstalledTarget(redacted)")
    }
}

impl InstalledTarget {
    /// Correlation for the retained native role; possession of these bytes
    /// alone cannot construct a role or authorize a credential read.
    pub fn context(&self) -> Result<String, Error> {
        Ok(self
            .installed
            .upgrade()
            .ok_or(Error::Stale)?
            .context
            .clone())
    }
    pub fn assignment_generation(&self) -> Result<u64, Error> {
        Ok(self
            .installed
            .upgrade()
            .ok_or(Error::Stale)?
            .assignment
            .generation())
    }
    pub fn target_ref(&self) -> Result<String, Error> {
        let installed = self.installed.upgrade().ok_or(Error::Stale)?;
        Ok(crate::harness::codex_enrollment::account_id(
            &installed.assignment.binding().request.label,
        ))
    }
}

/// Borrowed exclusion + exact retained target + actual physical B resources.
/// Constructed only inside the registry's same-Cell installation driver.
pub(in crate::harness) struct TargetAdmission<'a> {
    state: &'a Arc<Mutex<State>>,
    cell: &'a Arc<Mutex<Cell>>,
    kernel: &'a Arc<ReservationFamilyExclusive>,
    resource: &'a Arc<TargetFamilyLease>,
    intent: &'a assignment::prepare::TargetIntent,
    book_namespace: &'a assignment::BookNamespace,
}
impl TargetAdmission<'_> {
    pub(in crate::harness) fn intent(&self) -> &assignment::prepare::TargetIntent {
        self.intent
    }
    pub(in crate::harness) fn book_namespace(&self) -> &assignment::BookNamespace {
        self.book_namespace
    }
    pub(in crate::harness) fn matches_home(&self, home: &std::path::Path) -> bool {
        self.resource.gate_identity().matches_home(home)
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        self.kernel.current().map_err(str::to_owned)?;
        self.resource.current().map_err(str::to_owned)?;
        if !self.resource.matches_target(self.intent) {
            return Err("target family changed".into());
        }
        let state = lock(self.state).map_err(|_| "target registry busy")?;
        same_cell(&state, self.cell, self.intent.caller())
            .map_err(|_| "target reservation changed")?;
        let cell = lock(self.cell).map_err(|_| "target reservation busy")?;
        if !cell.target.matches_intent(self.intent)
            || !matches!(&cell.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, self.kernel))
            || !cell
                .target_resource
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, self.resource))
            || !state.app_lifetime.as_ref().is_some_and(|app| {
                cell._scope
                    .as_ref()
                    .is_some_and(|scope| scope.cancellation().owned_by(app))
            })
        {
            return Err("target reservation changed".into());
        }
        Ok(())
    }
    pub(in crate::harness) fn permits_new_effect(&self) -> Result<(), String> {
        self.current()?;
        let state = lock(self.state).map_err(|_| "target registry busy")?;
        let cell = lock(self.cell).map_err(|_| "target reservation busy")?;
        reservation_cell_scope(&state, &cell).map_err(|_| "target operation cancelled".into())
    }
}
struct Driver(Arc<AtomicBool>);
impl Driver {
    fn acquire(flag: Arc<AtomicBool>) -> Result<Self, Error> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(Self(flag))
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Reservation {
    /// Install only the immutable prepared target. All account/owner/Book/Slot
    /// claims are released before a destination SDK factory may begin loading.
    pub fn install_target(
        &self,
        env: &Env,
        peer: &Peer,
        sealed: &SourceSeal,
        boundary: &str,
    ) -> Result<InstalledTarget, Error> {
        if !hex(boundary, 32) || !peer.current() {
            return Err(Error::Denied);
        }
        let state = self.state.upgrade().ok_or(Error::Stale)?;
        let kernel = sealed.retained_kernel().ok_or(Error::Stale)?;
        kernel.current().map_err(|_| Error::Stale)?;
        let (intent, namespace_source, existing, driver) = {
            let state = lock(&state)?;
            same_cell(&state, &self.cell, &peer.caller)?;
            let cell = lock(&self.cell)?;
            if cell.process != peer.process
                || cell.target.caller() != &peer.caller
                || !matches!(&cell.phase, seal::Phase::Sealed { kernel: current, .. } if Arc::ptr_eq(current, &kernel))
            {
                return Err(Error::Stale);
            }
            if cell.target_resource.is_none() {
                reservation_cell_scope(&state, &cell)?;
            }
            (
                cell.target.intent(),
                Arc::clone(cell.origin.source()),
                cell.target_resource.clone(),
                cell.target_driver.clone(),
            )
        };
        let book_namespace = namespace_source
            .namespaces()
            .ok_or(Error::Unavailable)?
            .book();
        let _driver = Driver::acquire(driver)?;
        let resource = match existing {
            Some(resource) => resource,
            None => {
                let resource = Arc::new(kernel.target_resource(&intent).map_err(|error| {
                    if error.contains("busy") {
                        Error::Busy
                    } else {
                        Error::Unavailable
                    }
                })?);
                let state = lock(&state)?;
                same_cell(&state, &self.cell, &peer.caller)?;
                let mut cell = lock(&self.cell)?;
                reservation_cell_scope(&state, &cell)?;
                if !cell.target.matches_intent(&intent)
                    || cell.target_resource.is_some()
                    || !matches!(&cell.phase, seal::Phase::Sealed { kernel: current, .. } if Arc::ptr_eq(current, &kernel))
                {
                    return Err(Error::Stale);
                }
                // Strong custody precedes every canonical assignment effect.
                cell.target_resource = Some(resource.clone());
                resource
            }
        };
        let admission = TargetAdmission {
            state: &state,
            cell: &self.cell,
            kernel: &kernel,
            resource: &resource,
            intent: &intent,
            book_namespace,
        };
        let (source, receipt, commit) =
            assignment::prepare::install::install(env, &admission, boundary).map_err(|error| {
                if error.contains("busy") {
                    Error::Busy
                } else {
                    Error::Stale
                }
            })?;
        // Entropy generation is outside the registry mutex. A lost reply
        // reuses the existing retained entry rather than this fresh candidate.
        let context = aterm_uds::rand::hex_token::<32>().map_err(|_| Error::Unavailable)?;
        admission.current().map_err(|_| Error::Stale)?;
        if source.binding() != intent.target_binding().ok_or(Error::Stale)?
            || Some(source.assignment_generation()) != intent.assignment_generation()
            || source.native_intent() != Some(intent.operation())
        {
            return Err(Error::Stale);
        }
        let installed = {
            let state = lock(&state)?;
            same_cell(&state, &self.cell, &peer.caller)?;
            let mut cell = lock(&self.cell)?;
            if !cell.target.matches_intent(&intent)
                || !matches!(&cell.phase, seal::Phase::Sealed { kernel: current, .. } if Arc::ptr_eq(current, &kernel))
            {
                return Err(Error::Stale);
            }
            if let Some(installed) = &cell.installed {
                if installed.boundary != boundary
                    || !same_source_scope(&installed.source, &source)
                    || !revision_descends(
                        source.credential_generation(),
                        source.digest(),
                        installed.source.credential_generation(),
                        installed.source.digest(),
                    )
                {
                    return Err(Error::Stale);
                }
                installed.clone()
            } else {
                let installed = Arc::new(Installed {
                    context,
                    source: Arc::new(source),
                    assignment: Arc::new(commit),
                    resource,
                    boundary: boundary.into(),
                });
                cell.installed = Some(installed.clone());
                installed
            }
        };
        // SDK readers start only after exact private Slot claims have retired.
        drop(receipt);
        Ok(InstalledTarget {
            state: Arc::downgrade(&state),
            cell: Arc::downgrade(&self.cell),
            installed: Arc::downgrade(&installed),
        })
    }
}

struct Constructor {
    member: String,
    operation: String,
    scope: crate::harness::account_work::Scope,
    worker: AtomicBool,
}
pub(in crate::harness) struct ConstructorEntry {
    work: Arc<Constructor>,
    connection: Connection,
    preload: Option<String>,
    ready: Option<ready::Body>,
    no_reader: Option<no_reader::Body>,
}
/// Accounted synchronously before the front door may expose Waiting. It owns
/// no credential permission until the worker validates the actual role again.
pub struct TargetConstruction {
    state: Weak<Mutex<State>>,
    cell: Weak<Mutex<Cell>>,
    installed: Weak<Installed>,
    kernel: Arc<ReservationFamilyExclusive>,
    work: Arc<Constructor>,
    connection: Connection,
    process: Process,
    preload: Option<String>,
    preload_attempt: Option<preload_sequence::Attempt>,
}
impl TargetConstruction {
    pub fn member(&self) -> &str {
        &self.work.member
    }
}
impl std::fmt::Debug for TargetConstruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetConstruction(redacted)")
    }
}
struct ConstructorWorker(Arc<Constructor>);
impl Drop for ConstructorWorker {
    fn drop(&mut self) {
        self.0.worker.store(false, Ordering::Release);
    }
}

/// Borrowed installed/read authority with the actual counted constructor work.
/// Unlike a signed record or SourceHandle it cannot be reconstructed from data.
pub(in crate::harness) struct InstalledRead<'a> {
    command: &'a TargetConstruction,
    installed: &'a Arc<Installed>,
    expected: &'a Arc<slot::SourceHandle>,
}
impl InstalledRead<'_> {
    pub(in crate::harness) fn caller(&self) -> &slot::Caller {
        self.installed.assignment.caller()
    }
    pub(in crate::harness) fn expected(&self) -> &slot::SourceHandle {
        self.expected
    }
    pub(in crate::harness) fn assignment(&self) -> &assignment::AssignmentCommit {
        &self.installed.assignment
    }
    pub(in crate::harness) fn gate(&self) -> &SourceGateIdentity {
        self.installed.resource.gate_identity()
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        self.command.kernel.current().map_err(str::to_owned)?;
        self.installed.resource.current().map_err(str::to_owned)?;
        if self.command.work.scope.cancelled() {
            return Err("target constructor cancelled".into());
        }
        let state = self
            .command
            .state
            .upgrade()
            .ok_or("target registry retired")?;
        let cell = self
            .command
            .cell
            .upgrade()
            .ok_or("target reservation retired")?;
        let state = lock(&state).map_err(|_| "target registry busy")?;
        same_cell(&state, &cell, self.caller()).map_err(|_| "target reservation changed")?;
        let cell = lock(&cell).map_err(|_| "target reservation busy")?;
        if !cell
            .installed
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, self.installed))
            || !matches!(&cell.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, &self.command.kernel))
            || !cell
                .target_constructors
                .get(&self.command.work.operation)
                .is_some_and(|entry| {
                    Arc::ptr_eq(&entry.work, &self.command.work)
                        && entry.connection.same(&self.command.connection)
                        && entry.preload == self.command.preload
                })
            || !self.command.connection.is_live()
            || !state
                .app_lifetime
                .as_ref()
                .is_some_and(|app| self.command.work.scope.cancellation().owned_by(app))
        {
            return Err("target constructor changed".into());
        }
        let actor = state
            .actors
            .get(&actor_key(self.caller()))
            .ok_or("target actor changed")?;
        if actor.process != self.command.process
            || !actor
                .issued
                .get(&self.command.work.member)
                .is_some_and(|issued| {
                    issued.operation == self.command.work.operation
                        && issued.connection.same(&self.command.connection)
                        && issued
                            .target
                            .as_ref()
                            .is_some_and(|target| target.ptr_eq(&Arc::downgrade(self.installed)))
                })
        {
            return Err("target member changed".into());
        }
        Ok(())
    }
}

pub(super) fn derived_open_is_closing(
    state: &State,
    caller: &slot::Caller,
    operation: &str,
) -> bool {
    state
        .target_opens
        .get(&open_key(caller, operation))
        .is_some_and(|record| {
            record.cleanup.is_some()
                || record.closed.is_some()
                || record.member_retirement.is_some()
        })
}

pub enum TargetOpenPreparation {
    Pending(Box<TargetConstruction>),
    Settled(MemberAllocation),
}
pub enum TargetPreloadPreparation {
    Pending(TargetConstruction),
    Settled(Preload),
}

enum ConstructorOrigin<'a> {
    Explicit,
    Derived(&'a SourceUse),
    // Existing native OPEN evidence, never a new allocation/body.
    Accepted,
}

impl Registry {
    /// The front door supplies the actual socket caller. A lost OPEN reply is
    /// resolved from exact acceptance evidence, without any source read.
    pub fn prepare_target_open_cleanup(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        operation: &str,
        open_operation: &str,
        member: Option<&str>,
    ) -> Result<TargetOpenCleanup, Error> {
        self.prepare_target_open_cleanup_bound(
            caller,
            connection,
            operation,
            open_operation,
            member,
            None,
        )
    }

    pub fn prepare_target_open_retirement(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        request: TargetOpenRetirement<'_>,
    ) -> Result<TargetOpenCleanup, Error> {
        if !hex(request.assignment, 32) || !hex(request.context, 64) {
            return Err(Error::Denied);
        }
        self.prepare_target_open_cleanup_bound(
            caller,
            connection,
            request.operation,
            request.open_operation,
            request.member,
            Some((request.assignment, request.context)),
        )
    }

    fn prepare_target_open_cleanup_bound(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        operation: &str,
        open_operation: &str,
        member: Option<&str>,
        context: Option<(&str, &str)>,
    ) -> Result<TargetOpenCleanup, Error> {
        if !connection.is_live()
            || !hex(operation, 32)
            || !hex(open_operation, 32)
            || member.is_some_and(|value| !hex(value, 32))
        {
            return Err(Error::Denied);
        }
        let request = OpenClosure {
            operation: operation.into(),
            member: member.map(str::to_owned),
            context: context.map(|(assignment, context)| (assignment.into(), context.into())),
        };
        let (process, scope, turn) = {
            let state = lock(&self.state)?;
            let record = state
                .target_opens
                .get(&open_key(caller, open_operation))
                .ok_or(Error::Stale)?;
            if record.caller != *caller
                || member.is_some_and(|value| value != record.member)
                || context.is_some_and(|(assignment, context)| {
                    record.assignment != assignment || record.context != context
                })
            {
                return Err(Error::Stale);
            }
            if let Some(closed) = &record.closed {
                if closed != &request {
                    return Err(Error::Stale);
                }
                (record.process.clone(), None, None)
            } else {
                // Once a credential read was admitted, only its separate exact
                // preload/member retirement route may settle that custody.
                if !record.preloads.is_empty() {
                    return Err(Error::Stale);
                }
                if let Some(completion) = &record.no_reader {
                    if !completion.retirement.retired() {
                        return Err(Error::Busy);
                    }
                    resources::check_retirement(&state, caller, &record.member)?;
                    let turn = match &record.cleanup_turn {
                        Some(turn) => turn.clone(),
                        None => Arc::new(
                            state
                                .app_lifetime
                                .as_ref()
                                .and_then(|app| app.release_turn())
                                .ok_or(Error::Busy)?,
                        ),
                    };
                    (record.process.clone(), None, Some(turn))
                } else {
                    let cell = state
                        .reservations
                        .get(&actor_key(caller))
                        .ok_or(Error::Stale)?;
                    let held = lock(cell)?;
                    let entry = held
                        .target_constructors
                        .get(open_operation)
                        .ok_or(Error::Stale)?;
                    if entry.work.member != record.member || entry.preload.is_some() {
                        return Err(Error::Stale);
                    }
                    (record.process.clone(), Some(entry.work.scope.clone()), None)
                }
            }
        };
        // Kernel/image observation is outside State. Foreground, work-connection
        // EOF and ordinary App admission are deliberately not cleanup gates.
        if Process::observe(caller.pid).as_ref() != Some(&process) {
            return Err(Error::Stale);
        }
        {
            let mut state = lock(&self.state)?;
            let key = open_key(caller, open_operation);
            if state.target_opens.iter().any(|(other, record)| {
                other != &key
                    && record.caller == *caller
                    && record
                        .cleanup
                        .as_ref()
                        .is_some_and(|old| old.operation == operation)
            }) {
                return Err(Error::Stale);
            }
            let record = state.target_opens.get_mut(&key).ok_or(Error::Stale)?;
            if record.process != process
                || context.is_some_and(|(assignment, context)| {
                    record.assignment != assignment || record.context != context
                })
                || !record.preloads.is_empty()
                || record.cleanup.as_ref().is_some_and(|old| old != &request)
            {
                return Err(Error::Stale);
            }
            if record.closed.is_none() {
                record.cleanup = Some(request.clone());
                if record.cleanup_turn.is_none() {
                    record.cleanup_turn = turn.clone();
                }
            } else if record.cleanup.as_ref() != Some(&request) {
                return Err(Error::Stale);
            }
        }
        let cleanup = TargetOpenCleanup {
            state: Arc::downgrade(&self.state),
            caller: caller.clone(),
            process,
            connection: connection.clone(),
            open: open_operation.into(),
            request,
            _scope: scope,
            _turn: turn,
        };
        self.check_target_open_cleanup(&cleanup)?;
        Ok(cleanup)
    }

    fn check_target_open_cleanup(&self, cleanup: &TargetOpenCleanup) -> Result<(), Error> {
        let state = cleanup.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state)
            || !cleanup.connection.is_live()
            || Process::observe(cleanup.caller.pid).as_ref() != Some(&cleanup.process)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }

    pub fn retire_target_open(&self, cleanup: TargetOpenCleanup) -> Result<(), Error> {
        self.check_target_open_cleanup(&cleanup)?;
        if let Some(retired) = no_reader::retire_completed(self, &cleanup)? {
            drop(retired);
            return Ok(());
        }
        let retired = {
            let mut state = lock(&self.state)?;
            let key = open_key(&cleanup.caller, &cleanup.open);
            let record = state.target_opens.get(&key).ok_or(Error::Stale)?;
            if record.caller != cleanup.caller
                || record.cleanup.as_ref() != Some(&cleanup.request)
                || record.process != cleanup.process
                || cleanup
                    .request
                    .context
                    .as_ref()
                    .is_some_and(|(assignment, context)| {
                        &record.assignment != assignment || &record.context != context
                    })
                || cleanup
                    .request
                    .member
                    .as_ref()
                    .is_some_and(|member| member != &record.member)
            {
                return Err(Error::Stale);
            }
            if let Some(closed) = &record.closed {
                return if closed == &cleanup.request {
                    Ok(())
                } else {
                    Err(Error::Stale)
                };
            }
            if !record.preloads.is_empty() || cleanup._scope.is_none() {
                return Err(Error::Stale);
            }
            let member = record.member.clone();
            if state
                .members
                .contains_key(&member_key(&cleanup.caller, &member)?)
            {
                return Err(Error::Stale);
            }
            let cell = state
                .reservations
                .get(&actor_key(&cleanup.caller))
                .ok_or(Error::Stale)?
                .clone();
            let mut held = lock(&cell)?;
            let entry = held
                .target_constructors
                .get(&cleanup.open)
                .ok_or(Error::Stale)?;
            if entry.work.member != member
                || entry.preload.is_some()
                || entry.work.worker.load(Ordering::Acquire)
                || Arc::strong_count(&entry.work) != 1
            {
                return Err(Error::Busy);
            }
            // Publish the exact fence before releasing any counted custody.
            // All later allocation/preload paths consult this same ledger.
            state.target_opens.get_mut(&key).ok_or(Error::Stale)?.closed =
                Some(cleanup.request.clone());
            if let Some(actor) = state.actors.get_mut(&actor_key(&cleanup.caller)) {
                actor.issued.remove(&member);
            }
            held.target_constructors.remove(&cleanup.open)
        };
        drop(retired);
        Ok(())
    }

    /// Resolve only a still-retained installation. Neither an operation nor a
    /// context string reconstructs exclusion, target admission, or a new role.
    pub fn installed_target(
        &self,
        peer: &Peer,
        operation: &str,
        context: Option<&str>,
    ) -> Result<InstalledTarget, Error> {
        if !hex(operation, 32) || context.is_some_and(|value| !hex(value, 64)) || !peer.current() {
            return Err(Error::Denied);
        }
        let (cell, installed, kernel) = {
            let state = lock(&self.state)?;
            let cell = state
                .reservations
                .get(&actor_key(&peer.caller))
                .ok_or(Error::Stale)?
                .clone();
            let held = lock(&cell)?;
            if held.process != peer.process || held.target.operation() != operation {
                return Err(Error::Stale);
            }
            let installed = held.installed.clone().ok_or(Error::Stale)?;
            if context.is_some_and(|value| value != installed.context) {
                return Err(Error::Stale);
            }
            let kernel = match &held.phase {
                seal::Phase::Sealed { kernel, .. } => kernel.clone(),
                _ => return Err(Error::Stale),
            };
            drop(held);
            (cell, installed, kernel)
        };
        kernel.current().map_err(|_| Error::Stale)?;
        installed.resource.current().map_err(|_| Error::Stale)?;
        let state = lock(&self.state)?;
        same_cell(&state, &cell, &peer.caller)?;
        let held = lock(&cell)?;
        if held.process != peer.process
            || !held
                .installed
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &installed))
            || !matches!(&held.phase, seal::Phase::Sealed { kernel: current, .. } if Arc::ptr_eq(current, &kernel))
        {
            return Err(Error::Stale);
        }
        Ok(InstalledTarget {
            state: Arc::downgrade(&self.state),
            cell: Arc::downgrade(&cell),
            installed: Arc::downgrade(&installed),
        })
    }

    pub(super) fn open_installed_derived(
        &self,
        peer: &Peer,
        connection: &Connection,
        operation: &str,
        context: &SourceUse,
    ) -> Result<Option<MemberAllocation>, Error> {
        let role = {
            let state = lock(&self.state)?;
            let active = namespace::admitted(&state, context)?.0;
            let actor = state
                .actors
                .get(&actor_key(&peer.caller))
                .ok_or(Error::Stale)?;
            let Some(installed) = actor
                .issued
                .get(&context.member)
                .and_then(|issued| issued.target.as_ref())
                .and_then(Weak::upgrade)
            else {
                return Ok(None);
            };
            if normalize::lookup(&state, &peer.caller, &installed).is_some() {
                return Ok(None);
            }
            let cell = state
                .reservations
                .get(&actor_key(&peer.caller))
                .ok_or(Error::Stale)?;
            let held = lock(cell)?;
            if context.caller != peer.caller
                || actor.process != peer.process
                || !same_source_scope(&active.source, &installed.source)
                || held
                    .installed
                    .as_ref()
                    .is_none_or(|actual| !Arc::ptr_eq(actual, &installed))
                || !matches!(&held.phase, seal::Phase::Sealed { .. })
            {
                return Err(Error::Stale);
            }
            if !held.resource_phase.permits_factory() {
                return Err(Error::Busy);
            }
            InstalledTarget {
                state: Arc::downgrade(&self.state),
                cell: Arc::downgrade(cell),
                installed: Arc::downgrade(&installed),
            }
        };
        let command = self.prepare_target_constructor_from(
            peer,
            connection,
            &role,
            operation,
            ConstructorOrigin::Derived(context),
        )?;
        self.run_target_open(command).map(Some)
    }

    /// Data-only admission of a target OPEN before transport Waiting. Actual
    /// process and kernel-resource validation stays in the counted worker.
    pub fn prepare_target_open(
        &self,
        peer: &Peer,
        connection: &Connection,
        assignment: &str,
        context: &str,
        operation: &str,
    ) -> Result<TargetOpenPreparation, Error> {
        if !hex(assignment, 32) || !hex(context, 64) {
            return Err(Error::Denied);
        }
        let role = {
            let state = lock(&self.state)?;
            let cell = state
                .reservations
                .get(&actor_key(&peer.caller))
                .ok_or(Error::Stale)?;
            let held = lock(cell)?;
            let installed = held.installed.as_ref().ok_or(Error::Stale)?;
            if held.process != peer.process
                || held.target.operation() != assignment
                || installed.context != context
                || !matches!(&held.phase, seal::Phase::Sealed { .. })
            {
                return Err(Error::Stale);
            }
            if let Some(record) = state.target_opens.get(&open_key(&peer.caller, operation)) {
                if record.context != context
                    || record.process != peer.process
                    || record.parent_use.is_some()
                    || record.cleanup.is_some()
                    || record.closed.is_some()
                    || record.member_retirement.is_some()
                {
                    return Err(Error::Stale);
                }
                let issued = state
                    .actors
                    .get(&actor_key(&peer.caller))
                    .and_then(|actor| actor.issued.get(&record.member))
                    .ok_or(Error::Stale)?;
                if issued.operation != operation
                    || issued.parent_use.is_some()
                    || issued
                        .target
                        .as_ref()
                        .is_none_or(|target| !target.ptr_eq(&Arc::downgrade(installed)))
                {
                    return Err(Error::Stale);
                }
                if record.validated {
                    return Ok(TargetOpenPreparation::Settled(MemberAllocation {
                        member: record.member.clone(),
                        operation: operation.into(),
                    }));
                }
            }
            InstalledTarget {
                state: Arc::downgrade(&self.state),
                cell: Arc::downgrade(cell),
                installed: Arc::downgrade(installed),
            }
        };
        self.prepare_target_constructor(peer, connection, &role, operation)
            .map(|command| TargetOpenPreparation::Pending(Box::new(command)))
    }

    pub fn run_target_open(&self, command: TargetConstruction) -> Result<MemberAllocation, Error> {
        if command.preload.is_some() {
            return Err(Error::Denied);
        }
        let installed = command.installed.upgrade().ok_or(Error::Stale)?;
        if Process::observe(installed.assignment.caller().pid).as_ref() != Some(&command.process) {
            return Err(Error::Stale);
        }
        let state = command.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state) {
            return Err(Error::Stale);
        }
        let read = InstalledRead {
            command: &command,
            installed: &installed,
            expected: &installed.source,
        };
        read.current().map_err(|_| Error::Stale)?;
        {
            let mut state = lock(&self.state)?;
            let cell = command.cell.upgrade().ok_or(Error::Stale)?;
            same_cell(&state, &cell, installed.assignment.caller())?;
            let held = lock(&cell)?;
            if !held
                .target_constructors
                .get(&command.work.operation)
                .is_some_and(|entry| {
                    Arc::ptr_eq(&entry.work, &command.work)
                        && entry.connection.same(&command.connection)
                        && entry.preload == command.preload
                })
                || command.work.scope.cancelled()
                || !command.connection.is_live()
            {
                return Err(Error::Stale);
            }
            let record = state
                .target_opens
                .get_mut(&open_key(
                    installed.assignment.caller(),
                    &command.work.operation,
                ))
                .ok_or(Error::Stale)?;
            if record.context != installed.context
                || record.member != command.work.member
                || record.cleanup.is_some()
                || record.closed.is_some()
                || record.member_retirement.is_some()
            {
                return Err(Error::Stale);
            }
            record.validated = true;
        }
        Ok(MemberAllocation {
            member: command.work.member.clone(),
            operation: command.work.operation.clone(),
        })
    }

    /// Native allocation and counted custody precede every physical probe/read.
    /// Only an actual installed role can enter this destination factory lane.
    pub fn open_target(
        &self,
        peer: &Peer,
        connection: &Connection,
        role: &InstalledTarget,
        operation: &str,
    ) -> Result<MemberAllocation, Error> {
        if !hex(operation, 32) || !connection.is_live() || !peer.current() {
            return Err(Error::Denied);
        }
        let installed = role.installed.upgrade().ok_or(Error::Stale)?;
        let cell = role.cell.upgrade().ok_or(Error::Stale)?;
        let owner = role.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&owner, &self.state) {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            same_cell(&state, &cell, &peer.caller)?;
            let held = lock(&cell)?;
            if held.process != peer.process
                || !held
                    .installed
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &installed))
            {
                return Err(Error::Stale);
            }
            if let Some(record) = state.target_opens.get(&open_key(&peer.caller, operation)) {
                if record.process != peer.process
                    || record.parent_use.is_some()
                    || record.context != installed.context
                    || record.cleanup.is_some()
                    || record.closed.is_some()
                    || record.member_retirement.is_some()
                {
                    return Err(Error::Stale);
                }
                let issued = state
                    .actors
                    .get(&actor_key(&peer.caller))
                    .and_then(|actor| actor.issued.get(&record.member))
                    .ok_or(Error::Stale)?;
                if issued.operation != operation
                    || !issued
                        .target
                        .as_ref()
                        .is_some_and(|target| target.ptr_eq(&Arc::downgrade(&installed)))
                {
                    return Err(Error::Stale);
                }
                // Correlation never creates another Scope after a completed
                // constructor. Credential reads have their own PRELOAD turn.
                if record.validated {
                    return Ok(MemberAllocation {
                        member: record.member.clone(),
                        operation: operation.into(),
                    });
                }
            }
        }
        let command = self.prepare_target_constructor(peer, connection, role, operation)?;
        self.run_target_open(command)
    }

    pub fn prepare_target_constructor(
        &self,
        peer: &Peer,
        connection: &Connection,
        role: &InstalledTarget,
        operation: &str,
    ) -> Result<TargetConstruction, Error> {
        self.prepare_target_constructor_from(
            peer,
            connection,
            role,
            operation,
            ConstructorOrigin::Explicit,
        )
    }

    fn prepare_target_constructor_from(
        &self,
        peer: &Peer,
        connection: &Connection,
        role: &InstalledTarget,
        operation: &str,
        origin: ConstructorOrigin<'_>,
    ) -> Result<TargetConstruction, Error> {
        if !hex(operation, 32) || !connection.is_live() {
            return Err(Error::Denied);
        }
        let role_state = role.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&role_state, &self.state) {
            return Err(Error::Stale);
        }
        let cell = role.cell.upgrade().ok_or(Error::Stale)?;
        let installed = role.installed.upgrade().ok_or(Error::Stale)?;
        let mut state = lock(&self.state)?;
        same_cell(&state, &cell, &peer.caller)?;
        let mut held = lock(&cell)?;
        if held.process != peer.process
            || installed.assignment.caller() != &peer.caller
            || !held
                .installed
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &installed))
        {
            return Err(Error::Stale);
        }
        reservation_cell_scope(&state, &held)?;
        let kernel = match &held.phase {
            seal::Phase::Sealed { kernel, .. } => kernel.clone(),
            _ => return Err(Error::Stale),
        };
        let actor = state
            .actors
            .get(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?;
        if actor.process != peer.process {
            return Err(Error::Stale);
        }
        let parent = match origin {
            ConstructorOrigin::Explicit => None,
            ConstructorOrigin::Derived(context) => {
                let active = namespace::admitted(&state, context)?.0;
                if context.caller != peer.caller
                    || !same_source_scope(&active.source, &installed.source)
                    || active._scope.cancelled()
                    || state
                        .app_lifetime
                        .as_ref()
                        .is_none_or(|app| !active._scope.cancellation().owned_by(app))
                    || actor
                        .issued
                        .get(&context.member)
                        .and_then(|issued| issued.target.as_ref())
                        .is_none_or(|target| !target.ptr_eq(&Arc::downgrade(&installed)))
                {
                    return Err(Error::Stale);
                }
                Some(parent_use(context))
            }
            ConstructorOrigin::Accepted => state
                .target_opens
                .get(&open_key(&peer.caller, operation))
                .ok_or(Error::Stale)?
                .parent_use
                .clone(),
        };
        let ledger_key = open_key(&peer.caller, operation);
        if let Some(record) = state.target_opens.get(&ledger_key) {
            if record.caller != peer.caller
                || record.process != peer.process
                || record.context != installed.context
                || record.parent_use != parent
                || record.cleanup.is_some()
                || record.closed.is_some()
                || record.member_retirement.is_some()
            {
                return Err(Error::Stale);
            }
        } else if state.target_opens.len() >= MAX_ACTORS * MAX_MEMBERS
            || state
                .target_opens
                .values()
                .filter(|entry| entry.caller == peer.caller)
                .count()
                >= MAX_MEMBERS
        {
            return Err(Error::Busy);
        }
        let existing = actor
            .issued
            .iter()
            .find(|(_, issued)| issued.operation == operation);
        let member = if let Some((member, issued)) = existing {
            if issued.parent_use != parent
                || !issued
                    .target
                    .as_ref()
                    .is_some_and(|target| target.ptr_eq(&Arc::downgrade(&installed)))
                || (!issued.connection.same(connection) && issued.connection.is_live())
            {
                return Err(Error::Stale);
            }
            member.clone()
        } else {
            if !held.resource_phase.permits_factory() {
                return Err(Error::Busy);
            }
            if held.target_issued.values().any(|old| old == operation) {
                return Err(Error::Stale);
            }
            if held.target_issued.len() >= MAX_MEMBERS || actor.issued.len() >= MAX_MEMBERS {
                return Err(Error::Busy);
            }
            let ordinal = actor.highwater.checked_add(1).ok_or(Error::Unavailable)?;
            format!("{}{:016x}", actor.epoch, ordinal)
        };
        if state
            .target_opens
            .get(&ledger_key)
            .is_some_and(|record| record.member != member)
        {
            return Err(Error::Stale);
        }
        if let Some(entry) = held.target_constructors.get(operation)
            && (entry.work.member != member
                || (entry.work.worker.load(Ordering::Acquire)
                    && !entry.connection.same(connection)))
        {
            return Err(Error::Busy);
        }
        let work = if let Some(entry) = held.target_constructors.get_mut(operation) {
            entry.connection = connection.clone();
            entry.work.clone()
        } else {
            if !held.resource_phase.permits_factory() {
                return Err(Error::Busy);
            }
            let scope = state
                .app_lifetime
                .as_ref()
                .and_then(|app| app.enter())
                .ok_or(Error::Busy)?;
            let work = Arc::new(Constructor {
                member: member.clone(),
                operation: operation.into(),
                scope,
                worker: false.into(),
            });
            held.target_constructors.insert(
                operation.into(),
                ConstructorEntry {
                    work: work.clone(),
                    connection: connection.clone(),
                    preload: None,
                    ready: None,
                    no_reader: None,
                },
            );
            work
        };
        let actor = state
            .actors
            .get_mut(&actor_key(&peer.caller))
            .ok_or(Error::Stale)?;
        if let Some(issued) = actor.issued.get_mut(&member) {
            issued.connection = connection.clone();
        } else {
            actor.highwater = actor.highwater.checked_add(1).ok_or(Error::Unavailable)?;
            actor.issued.insert(
                member.clone(),
                Issued {
                    operation: operation.into(),
                    connection: connection.clone(),
                    parent_use: parent.clone(),
                    target: Some(Arc::downgrade(&installed)),
                },
            );
            held.target_issued.insert(member, operation.into());
        }
        state
            .target_opens
            .entry(ledger_key)
            .or_insert_with(|| OpenRecord {
                caller: peer.caller.clone(),
                process: peer.process.clone(),
                context: installed.context.clone(),
                assignment: installed.assignment.operation().into(),
                member: work.member.clone(),
                parent_use: parent,
                validated: false,
                preloads: Default::default(),
                latest_preload: None,
                cleanup: None,
                closed: None,
                member_retirement: None,
                member_retired: false,
                no_reader: None,
                cleanup_turn: None,
            });
        Ok(TargetConstruction {
            state: Arc::downgrade(&self.state),
            cell: Arc::downgrade(&cell),
            installed: Arc::downgrade(&installed),
            kernel,
            work,
            connection: connection.clone(),
            process: peer.process.clone(),
            preload: None,
            preload_attempt: None,
        })
    }

    /// Admit an exact target PRELOAD before Waiting without a physical probe.
    /// A completed replay carries only its already-issued Source metadata.
    #[expect(
        clippy::too_many_arguments,
        reason = "caller, original connection, installed context and exact sequenced PRELOAD body remain explicit"
    )]
    pub fn prepare_target_preload_context(
        &self,
        peer: &Peer,
        connection: &Connection,
        assignment: &str,
        context: &str,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
    ) -> Result<TargetPreloadPreparation, Error> {
        if !hex(assignment, 32)
            || !hex(context, 64)
            || !hex(member, 32)
            || !hex(operation, 32)
            || !connection.is_live()
        {
            return Err(Error::Denied);
        }
        let role = {
            let state = lock(&self.state)?;
            let cell = state
                .reservations
                .get(&actor_key(&peer.caller))
                .ok_or(Error::Stale)?;
            let held = lock(cell)?;
            let installed = held.installed.as_ref().ok_or(Error::Stale)?;
            if held.process != peer.process
                || held.target.operation() != assignment
                || installed.context != context
                || !matches!(&held.phase, seal::Phase::Sealed { .. })
            {
                return Err(Error::Stale);
            }
            let attempt = preload_sequence::Attempt {
                sequence,
                operation: operation.into(),
                origin: preload_sequence::Origin::Target {
                    assignment: assignment.into(),
                    context: context.into(),
                },
            };
            if let Some(ledger) = state
                .preload_sequences
                .get(&member_key(&peer.caller, member)?)
            {
                ledger.check_attempt(&peer.process, &attempt)?;
            }
            let issued = state
                .actors
                .get(&actor_key(&peer.caller))
                .and_then(|actor| actor.issued.get(member))
                .ok_or(Error::Stale)?;
            if issued
                .target
                .as_ref()
                .is_none_or(|target| !target.ptr_eq(&Arc::downgrade(installed)))
            {
                return Err(Error::Stale);
            }
            let record = state
                .target_opens
                .get(&open_key(&peer.caller, &issued.operation))
                .ok_or(Error::Stale)?;
            if record.context != context || record.process != peer.process {
                return Err(Error::Stale);
            }
            record.admits_preload(member, operation)?;
            if record.preloads.contains(operation) {
                if let Some(loaded) = state.members.get(&member_key(&peer.caller, member)?)
                    && loaded.operation == operation
                    && !loaded.unknown
                    && loaded.connection.same(connection)
                    && (loaded.readiness.manager() || loaded.readiness.reader())
                    && same_source_scope(&loaded.source, &installed.source)
                {
                    return Ok(TargetPreloadPreparation::Settled(Preload {
                        member: member.into(),
                        operation: operation.into(),
                        registration: loaded.registration.clone(),
                        source: loaded.source.clone(),
                    }));
                }
                // Only the actual still-retained read may continue. Historical
                // admission metadata can never mint a new credential turn.
                if !held
                    .target_constructors
                    .get(&issued.operation)
                    .is_some_and(|entry| entry.preload.as_deref() == Some(operation))
                {
                    return Err(Error::Stale);
                }
            }
            InstalledTarget {
                state: Arc::downgrade(&self.state),
                cell: Arc::downgrade(cell),
                installed: Arc::downgrade(installed),
            }
        };
        self.prepare_target_preload(peer, connection, &role, member, operation, sequence)
            .map(TargetPreloadPreparation::Pending)
    }

    /// Bind a distinct credential-read attempt to an already allocated OPEN.
    /// The member ID only locates retained allocation evidence; it cannot
    /// create membership or retarget the installed assignment.
    pub fn prepare_target_preload(
        &self,
        peer: &Peer,
        connection: &Connection,
        role: &InstalledTarget,
        member: &str,
        operation: &str,
        sequence: PreloadSequence,
    ) -> Result<TargetConstruction, Error> {
        if !hex(member, 32) || !hex(operation, 32) {
            return Err(Error::Denied);
        }
        let open =
            {
                let state = lock(&self.state)?;
                if state
                    .unrelated
                    .contains_key(&member_key(&peer.caller, member)?)
                {
                    return Err(Error::Denied);
                }
                let actor = state
                    .actors
                    .get(&actor_key(&peer.caller))
                    .ok_or(Error::Stale)?;
                let open = actor
                    .issued
                    .get(member)
                    .ok_or(Error::Stale)?
                    .operation
                    .clone();
                state
                    .target_opens
                    .get(&open_key(&peer.caller, &open))
                    .ok_or(Error::Stale)?
                    .admits_preload(member, operation)?;
                if let Some(cell) = state.reservations.get(&actor_key(&peer.caller)) {
                    let held = lock(cell)?;
                    if held.target_constructors.get(&open).is_some_and(|entry| {
                        entry.preload.as_ref().is_some_and(|old| old != operation)
                    }) {
                        return Err(Error::Busy);
                    }
                }
                open
            };
        if open == operation {
            return Err(Error::Denied);
        }
        let mut command = self.prepare_target_constructor_from(
            peer,
            connection,
            role,
            &open,
            ConstructorOrigin::Accepted,
        )?;
        let mut state = lock(&self.state)?;
        let cell = command.cell.upgrade().ok_or(Error::Stale)?;
        same_cell(&state, &cell, &peer.caller)?;
        let mut held = lock(&cell)?;
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.into(),
            origin: preload_sequence::Origin::Target {
                assignment: held.target.operation().into(),
                context: held.installed.as_ref().ok_or(Error::Stale)?.context.clone(),
            },
        };
        let record = state
            .target_opens
            .get(&open_key(&peer.caller, &open))
            .ok_or(Error::Stale)?;
        record.admits_preload(member, operation)?;
        let entry = held
            .target_constructors
            .get_mut(&open)
            .ok_or(Error::Stale)?;
        if entry.work.member != member
            || !Arc::ptr_eq(&entry.work, &command.work)
            || !entry.connection.same(connection)
            || entry.preload.as_ref().is_some_and(|old| old != operation)
            || entry.no_reader.is_some()
        {
            return Err(Error::Stale);
        }
        preload_sequence::accept(
            &mut state,
            &member_key(&peer.caller, member)?,
            &peer.process,
            &attempt,
            &entry.work.scope,
        )?;
        let record = state
            .target_opens
            .get_mut(&open_key(&peer.caller, &open))
            .ok_or(Error::Stale)?;
        entry.preload = Some(operation.into());
        record.preloads.insert(operation.into());
        record.latest_preload = Some(operation.into());
        command.preload = Some(operation.into());
        command.preload_attempt = Some(attempt);
        Ok(command)
    }

    pub fn preload_target(&self, env: &Env, command: TargetConstruction) -> Result<Preload, Error> {
        let preload_operation = command.preload.as_deref().ok_or(Error::Denied)?;
        let attempt = command.preload_attempt.as_ref().ok_or(Error::Denied)?;
        let state = command.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state) {
            return Err(Error::Stale);
        }
        command
            .work
            .worker
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        let _worker = ConstructorWorker(command.work.clone());
        let installed = command.installed.upgrade().ok_or(Error::Stale)?;
        {
            let state = lock(&self.state)?;
            preload_sequence::check_current(
                &state,
                &member_key(installed.assignment.caller(), &command.work.member)?,
                &command.process,
                attempt,
            )?;
        }
        if observed(installed.assignment.caller())?.ne(&command.process) {
            return Err(Error::Stale);
        }
        let permission = InstalledRead {
            command: &command,
            installed: &installed,
            expected: &installed.source,
        };
        permission.current().map_err(|_| Error::Stale)?;
        let (source, receipt) =
            slot::preload_installed(env, &permission).map_err(|_| Error::Unavailable)?;
        permission.current().map_err(|_| Error::Stale)?;
        let key = member_key(installed.assignment.caller(), &command.work.member)?;
        let old = {
            let state = lock(&self.state)?;
            state.members.get(&key).map(|member| {
                (
                    member.source.clone(),
                    member.registration.clone(),
                    member.readiness.reader_owned,
                    member.unknown,
                    member.connection.clone(),
                )
            })
        };
        let reader_owned = old.as_ref().is_some_and(|old| old.2);
        let readiness = SourceReadiness::target(installed.resource.clone(), reader_owned)?;
        let source = Arc::new(source);
        let registration = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let result = {
            let mut state = lock(&self.state)?;
            preload_sequence::check_current(&state, &key, &command.process, attempt)?;
            let cell = command.cell.upgrade().ok_or(Error::Stale)?;
            same_cell(&state, &cell, installed.assignment.caller())?;
            let held = lock(&cell)?;
            if !held
                .installed
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &installed))
                || !held
                    .target_constructors
                    .get(&command.work.operation)
                    .is_some_and(|entry| {
                        Arc::ptr_eq(&entry.work, &command.work)
                            && entry.connection.same(&command.connection)
                    })
                || !command.connection.is_live()
            {
                return Err(Error::Stale);
            }
            if let Some(previous) = state.members.get(&key) {
                if old.as_ref().is_none_or(|old| {
                    !Arc::ptr_eq(&old.0, &previous.source) || old.1 != previous.registration
                }) || !same_source_scope(&previous.source, &source)
                    || !revision_descends(
                        source.credential_generation(),
                        source.digest(),
                        previous.source.credential_generation(),
                        previous.source.digest(),
                    )
                {
                    return Err(Error::Stale);
                }
                if previous.operation == preload_operation
                    && previous.connection.same(&command.connection)
                    && !previous.unknown
                {
                    return Ok(Preload {
                        member: command.work.member.clone(),
                        operation: preload_operation.into(),
                        registration: previous.registration.clone(),
                        source: previous.source.clone(),
                    });
                }
            } else if old.is_some() {
                return Err(Error::Stale);
            }
            let version = version(&mut state)?;
            state
                .preload_sequences
                .get_mut(&key)
                .ok_or(Error::Stale)?
                .loaded(
                    attempt,
                    &registration,
                    source.credential_generation(),
                    source.digest(),
                )?;
            state
                .preload_sequences
                .get_mut(&key)
                .ok_or(Error::Stale)?
                .record_initial(
                    attempt,
                    Preload {
                        member: command.work.member.clone(),
                        operation: preload_operation.into(),
                        registration: registration.clone(),
                        source: source.clone(),
                    },
                )?;
            state.members.insert(
                key,
                Member {
                    caller: installed.assignment.caller().clone(),
                    process: command.process.clone(),
                    operation: preload_operation.into(),
                    registration: registration.clone(),
                    connection: command.connection.clone(),
                    version,
                    source: source.clone(),
                    readiness,
                    unknown: false,
                    work_rebound: false,
                },
            );
            Preload {
                member: command.work.member.clone(),
                operation: preload_operation.into(),
                registration,
                source,
            }
        };
        drop(receipt);
        Ok(result)
    }
}

pub struct TargetReady<'a> {
    pub preload_sequence: PreloadSequence,
    pub assignment_generation: u64,
    pub member: &'a str,
    pub operation: &'a str,
    pub registration: &'a str,
    pub ready: Ready,
}
pub(in crate::harness::codex_source_registry) fn reported_role(
    ready: &Ready,
    source: &slot::SourceHandle,
) -> Result<ReadyRole, Error> {
    let (generation, digest, account, user, role) = match ready {
        Ready::Loaded {
            credential_generation,
            digest,
            account,
            user,
            auth,
        } => (
            *credential_generation,
            digest,
            account,
            user,
            ReadyRole::Manager(*auth),
        ),
        Ready::ReaderLoaded {
            credential_generation,
            digest,
            account,
            user,
        } => (
            *credential_generation,
            digest,
            account,
            user,
            ReadyRole::ReaderLoaded,
        ),
        Ready::PolicyUnavailable => return Ok(ReadyRole::PolicyUnavailable),
    };
    if generation != source.credential_generation()
        || digest != source.digest()
        || account != source.account()
        || user != source.user()
    {
        return Err(Error::Stale);
    }
    Ok(role)
}
impl Registry {
    /// Complete the exact accounted target preload; a reader has no invented
    /// manager counters. This is not an assignment or resource-readiness ACK.
    pub fn ready_target(
        &self,
        env: &Env,
        peer: &Peer,
        connection: &Connection,
        role: &InstalledTarget,
        report: TargetReady<'_>,
    ) -> Result<(), Error> {
        if !peer.current() || !connection.is_live() {
            return Err(Error::Stale);
        }
        let origin = role.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&origin, &self.state) {
            return Err(Error::Stale);
        }
        let installed = role.installed.upgrade().ok_or(Error::Stale)?;
        if installed.assignment.caller() != &peer.caller
            || installed.assignment.generation() != report.assignment_generation
        {
            return Err(Error::Stale);
        }
        match self.prepare_ready_report(
            &peer.caller,
            connection,
            installed.assignment.operation(),
            &installed.context,
            report,
        )? {
            TargetReadyPreparation::Pending(command) => self.run_target_ready(env, command),
            TargetReadyPreparation::Settled => Ok(()),
        }
    }
}

/// Data-only membership check under State. This never authenticates a target
/// from an ID; the weak target pointer must join the already installed Cell.
pub(super) fn allows_member(
    state: &State,
    cell: &Cell,
    caller: &slot::Caller,
    member: &str,
) -> bool {
    let Some(installed) = &cell.installed else {
        return false;
    };
    if installed.assignment.caller() != caller || !cell.target_issued.contains_key(member) {
        return false;
    }
    let Some(issued) = state
        .actors
        .get(&actor_key(caller))
        .and_then(|actor| actor.issued.get(member))
    else {
        return false;
    };
    if !issued
        .target
        .as_ref()
        .is_some_and(|target| target.ptr_eq(&Arc::downgrade(installed)))
    {
        return false;
    }
    member_key(caller, member)
        .ok()
        .and_then(|key| state.members.get(&key))
        .is_some_and(|current| {
            same_source_scope(&current.source, &installed.source)
                && revision_descends(
                    current.source.credential_generation(),
                    current.source.digest(),
                    installed.source.credential_generation(),
                    installed.source.digest(),
                )
        })
}
pub(in crate::harness) fn owners(state: &State) -> Vec<(slot::Caller, Process)> {
    state
        .reservations
        .values()
        .filter_map(|cell| {
            lock(cell)
                .ok()
                .map(|held| (held.target.caller().clone(), held.process.clone()))
        })
        .collect()
}

/// Only the kernel-proven dead-actor sweep calls this data-only hook. Exact
/// process equality prevents a later caller incarnation losing its custody.
/// No terminal OPEN/Slot/publication result is inferred from this cancellation.
pub(in crate::harness) fn revoke_dead_constructor_custody(
    state: &State,
    caller: &slot::Caller,
    expected: &Process,
) -> Result<Vec<ConstructorEntry>, Error> {
    let Some(cell) = state.reservations.get(&actor_key(caller)) else {
        return Ok(Vec::new());
    };
    let mut held = lock(cell)?;
    if held.process != *expected {
        return Err(Error::Stale);
    }
    held.target_retiring.retain(|witness| !witness.retired());
    if held
        .target_retiring
        .len()
        .checked_add(held.target_constructors.len())
        .is_none_or(|count| count > MAX_MEMBERS)
    {
        return Err(Error::Busy);
    }
    let entries = std::mem::take(&mut held.target_constructors);
    Ok(retire_constructor_entries(
        entries,
        &mut held.target_retiring,
    ))
}
fn retire_constructor_entries(
    entries: BTreeMap<String, ConstructorEntry>,
    witnesses: &mut Vec<crate::harness::account_work::Cancellation>,
) -> Vec<ConstructorEntry> {
    // At most one witness per accepted OPEN; the bounded history already owns
    // those entries. Keep unknown work instead of silently evicting a witness.
    let mut retired = Vec::with_capacity(entries.len());
    for entry in entries.into_values() {
        let witness = entry.work.scope.cancellation();
        witness.cancel();
        witnesses.push(witness);
        retired.push(entry);
    }
    retired
}
/// Kernel-departure cleanup only. The caller already proved exact process
/// death and settled separately counted use/preload/target captures. This does
/// not claim a cancelled/applied Book or authorize a successor consumer.
pub(in crate::harness) fn take_departed_custody(
    state: &State,
    caller: &slot::Caller,
    expected: &Process,
) -> Result<Option<RetiredCustody>, Error> {
    let Some(cell) = state.reservations.get(&actor_key(caller)) else {
        return Ok(None);
    };
    let mut held = lock(cell)?;
    if held.process != *expected
        || !held.target_constructors.is_empty()
        || held
            .target_retiring
            .iter()
            .any(|witness| !witness.retired())
    {
        return Err(Error::Busy);
    }
    if let Ok(scope) = held.scope() {
        scope.cancellation().cancel();
    }
    Ok(Some(held.take_custody()))
}

pub(in crate::harness) fn constructor_custody_pending(
    state: &State,
    caller: &slot::Caller,
) -> bool {
    state
        .reservations
        .get(&actor_key(caller))
        .is_some_and(|cell| {
            lock(cell).map_or(true, |held| {
                !held.target_constructors.is_empty()
                    || held
                        .target_retiring
                        .iter()
                        .any(|witness| !witness.retired())
            })
        })
}

/// Called only after exact explicit member/registration retirement checks and
/// settled native uses. A running worker keeps its actual Scope and resources.
pub(in crate::harness) fn retire_constructor(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> Result<Option<ConstructorEntry>, Error> {
    let Some(cell) = state.reservations.get(&actor_key(caller)) else {
        return Ok(None);
    };
    let mut held = lock(cell)?;
    let operation = held
        .target_constructors
        .iter()
        .find(|(_, entry)| entry.work.member == member)
        .map(|(operation, _)| operation.clone());
    let Some(operation) = operation else {
        return Ok(None);
    };
    if held.target_constructors[&operation]
        .work
        .worker
        .load(Ordering::Acquire)
        || Arc::strong_count(&held.target_constructors[&operation].work) != 1
    {
        return Err(Error::Busy);
    }
    Ok(held.target_constructors.remove(&operation))
}

#[cfg(all(test, unix))]
pub(super) fn absent_role_fixture() -> InstalledTarget {
    InstalledTarget {
        state: Weak::new(),
        cell: Weak::new(),
        installed: Weak::new(),
    }
}

#[cfg(all(test, unix))]
pub(super) fn open_cleanup_fixture(
    registry: &Registry,
    cell: &Arc<Mutex<Cell>>,
    peer: &Peer,
    connection: &Connection,
    allocation: (&str, &str),
    scope: &crate::harness::account_work::Scope,
    accepted_preload: bool,
) {
    let (member, operation) = allocation;
    // Ledger-lifecycle injection only. This mints no InstalledTarget, Source,
    // supported consumer proof, or credential/owner admission.
    let assignment = cell.lock().unwrap().target.operation().to_owned();
    let mut state = registry.state.lock().unwrap();
    state.target_opens.insert(
        open_key(&peer.caller, operation),
        OpenRecord {
            caller: peer.caller.clone(),
            process: peer.process.clone(),
            context: "c".repeat(64),
            assignment,
            member: member.into(),
            parent_use: None,
            validated: false,
            preloads: if accepted_preload {
                ["d".repeat(32)].into()
            } else {
                Default::default()
            },
            latest_preload: accepted_preload.then(|| "d".repeat(32)),
            cleanup: None,
            closed: None,
            member_retirement: None,
            member_retired: false,
            no_reader: None,
            cleanup_turn: None,
        },
    );
    let mut held = cell.lock().unwrap();
    held.target_constructors.insert(
        operation.into(),
        ConstructorEntry {
            work: Arc::new(Constructor {
                member: member.into(),
                operation: operation.into(),
                scope: scope.clone(),
                worker: false.into(),
            }),
            connection: connection.clone(),
            preload: accepted_preload.then(|| "d".repeat(32)),
            ready: None,
            no_reader: None,
        },
    );
}

#[cfg(all(test, unix))]
pub(super) fn member_retirement_fixture_state(
    registry: &Registry,
    caller: &slot::Caller,
    operation: &str,
) -> (bool, bool, bool) {
    let state = registry.state.lock().unwrap();
    let record = state
        .target_opens
        .get(&open_key(caller, operation))
        .unwrap();
    (
        record.member_retired,
        record.member_retirement.is_some(),
        record.cleanup.is_none() && record.closed.is_none(),
    )
}

#[cfg(all(test, unix))]
pub(super) fn captured_open_fixture(cell: &Arc<Mutex<Cell>>, operation: &str) -> impl Send {
    cell.lock().unwrap().target_constructors[operation]
        .work
        .clone()
}

#[cfg(test)]
#[path = "codex_source_target_retirement_tests.rs"]
mod retirement_tests;

#[path = "codex_source_work_rebind.rs"]
mod work_rebind;
pub use work_rebind::{WorkRebindKind, WorkRebindRequest};

#[path = "codex_source_target_resample.rs"]
mod resample;
pub use resample::{TargetResampleCommand, TargetResamplePreparation, TargetResampleRequest};

#[path = "codex_source_target_no_reader.rs"]
mod no_reader;
pub use no_reader::{TargetNoReaderCommand, TargetNoReaderPreparation, TargetNoReaderRequest};

pub(in crate::harness::codex_source_registry) use no_reader::take_departed_open_custody;
