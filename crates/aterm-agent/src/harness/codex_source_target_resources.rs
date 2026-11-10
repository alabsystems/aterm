// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Origin-specific resource readiness. This validates already loaded resources;
//! it cannot materialize a Slot, perform OAuth, or select an account.
use super::*;
#[path = "codex_source_target_ack.rs"]
mod ack;
use ack::AckWork;
pub(in crate::harness) use ack::TargetAckPermit;
pub use ack::{AckPreparation, TargetAckCommand, TargetApplied};

/// Native admission state, never reconstructed from a client phase or handle.
pub(in crate::harness) enum Phase {
    Initializing,
    Preparing { operation: String },
    Ready { operation: String },
    Acknowledging { operation: String },
    Applied { operation: String },
}
impl Phase {
    pub(in crate::harness) fn permits_factory(&self) -> bool {
        matches!(self, Self::Initializing)
    }
}

fn finish_phase(held: &mut Cell, operation: &str, ready: bool) {
    if matches!(&held.resource_phase, Phase::Preparing { operation: current } if current == operation)
    {
        held.resource_phase = if ready {
            Phase::Ready {
                operation: operation.into(),
            }
        } else {
            Phase::Initializing
        };
    }
}

pub struct ResourceReport<'a> {
    pub operation: &'a str,
    pub member: &'a str,
    pub registration: &'a str,
    pub boundary: &'a str,
    pub assignment_generation: u64,
    pub credential_generation: u64,
    pub digest: &'a str,
    pub account: &'a str,
    pub user: &'a str,
    pub auth: AuthState,
}

#[derive(Clone, PartialEq, Eq)]
struct Report {
    operation: String,
    member: String,
    registration: String,
    boundary: String,
    assignment_generation: u64,
    credential_generation: u64,
    digest: String,
    account: String,
    user: String,
    auth: AuthState,
}
impl Report {
    fn parse(report: ResourceReport<'_>) -> Result<Self, Error> {
        if ![
            report.operation,
            report.member,
            report.registration,
            report.boundary,
        ]
        .iter()
        .all(|value| hex(value, 32))
            || ![report.digest, report.account, report.user]
                .iter()
                .all(|value| hex(value, 64))
            || report.assignment_generation == 0
            || report.credential_generation == 0
        {
            return Err(Error::Denied);
        }
        Ok(Self {
            operation: report.operation.into(),
            member: report.member.into(),
            registration: report.registration.into(),
            boundary: report.boundary.into(),
            assignment_generation: report.assignment_generation,
            credential_generation: report.credential_generation,
            digest: report.digest.into(),
            account: report.account.into(),
            user: report.user.into(),
            auth: report.auth,
        })
    }
    fn matches_source(&self, source: &slot::SourceHandle) -> bool {
        self.assignment_generation == source.assignment_generation()
            && self.credential_generation == source.credential_generation()
            && self.digest == source.digest()
            && self.account == source.account()
            && self.user == source.user()
    }
}

/// The exact loaded destination cohort observed by the resource producer.
/// EOF does not alter this observation; an explicit retirement or a replacement
/// registration/readiness does. No lock or credential authority is stored here.
enum CohortMember {
    Loaded {
        member: String,
        registration: String,
        source: Arc<slot::SourceHandle>,
        role: ReadyRole,
    },
    NoNativeReader {
        member: String,
        registration: String,
        operation: String,
        process: Process,
        kind: NoNativeReaderKind,
    },
}
impl CohortMember {
    fn member(&self) -> &str {
        match self {
            Self::Loaded { member, .. } | Self::NoNativeReader { member, .. } => member,
        }
    }
}
fn cohort(state: &State, cell: &Cell, caller: &slot::Caller) -> Result<Vec<CohortMember>, Error> {
    let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
    let installed = cell.installed.as_ref().ok_or(Error::Stale)?;
    cell.target_issued
        .keys()
        .filter(|id| actor.issued.contains_key(*id))
        .map(|id| {
            let key = member_key(caller, id)?;
            if state.unrelated.contains_key(&key) {
                let value = super::no_reader::observed(state, caller, &cell.process, id, installed)
                    .ok_or(Error::Stale)?;
                return Ok(CohortMember::NoNativeReader {
                    member: id.clone(),
                    registration: value.registration.clone(),
                    operation: value.operation.clone(),
                    process: value.process.clone(),
                    kind: value.kind,
                });
            }
            let member = state.members.get(&key).ok_or(Error::Stale)?;
            let role = member.readiness.role.ok_or(Error::Stale)?;
            if matches!(role, ReadyRole::PolicyUnavailable) {
                return Err(Error::Stale);
            }
            Ok(CohortMember::Loaded {
                member: id.clone(),
                registration: member.registration.clone(),
                source: member.source.clone(),
                role,
            })
        })
        .collect()
}
fn cohort_current(state: &State, caller: &slot::Caller, recorded: &[CohortMember]) -> bool {
    recorded.iter().all(|prior| {
        let Ok(key) = member_key(caller, prior.member()) else {
            return false;
        };
        match prior {
            CohortMember::Loaded {
                registration,
                source,
                role,
                ..
            } => state.members.get(&key).is_some_and(|member| {
                member.registration == *registration
                    && member.readiness.role == Some(*role)
                    && same_source_scope(&member.source, source)
                    && member.source.credential_generation() == source.credential_generation()
                    && member.source.digest() == source.digest()
            }),
            CohortMember::NoNativeReader {
                registration,
                operation,
                process,
                kind,
                ..
            } => {
                // EOF alone does not revoke an issued resource observation.
                // Explicit cleanup and provider/registration replacement do.
                state.unrelated.get(&key).is_some_and(|actual| {
                    actual.registration == *registration
                        && actual.operation == *operation
                        && actual.process == *process
                        && actual.kind == *kind
                }) && !state.members.contains_key(&key)
                    && state
                        .target_opens
                        .get(&open_key(caller, operation))
                        .is_some_and(|open| {
                            open.cleanup.is_none()
                                && open.closed.is_none()
                                && open.member_retirement.is_none()
                                && open.no_reader.as_ref().is_some_and(|receipt| {
                                    receipt.body.registration == *registration
                                        && receipt.body.kind == *kind
                                        && receipt.retirement.retired()
                                })
                        })
            }
        }
    })
}

struct Work {
    scope: crate::harness::account_work::Scope,
    worker: AtomicBool,
}
pub(in crate::harness) struct Record {
    caller: slot::Caller,
    process: Process,
    assignment: String,
    context: String,
    report: Report,
    source: Arc<slot::SourceHandle>,
    connection: Connection,
    pending: Option<Arc<Work>>,
    work_identity: Weak<Work>,
    receipt: Option<String>,
    cohort: Vec<CohortMember>,
    refused: bool,
    revoked: bool,
    cancellation: crate::harness::account_work::Cancellation,
    ack: Option<Arc<AckWork>>,
    ack_retirement: Option<crate::harness::account_work::Cancellation>,
    pub(super) applied: Option<Arc<assignment::target_ack::AppliedReceipt>>,
    normalized: Option<Arc<super::normalize::NormalizationRetirement>>,
}

pub struct TargetResourceReceipt {
    token: String,
}
impl TargetResourceReceipt {
    pub fn token(&self) -> &str {
        &self.token
    }
}
impl std::fmt::Debug for TargetResourceReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetResourceReceipt(redacted)")
    }
}
pub enum ResourcePreparation {
    Ready(TargetResourceReceipt),
    Pending(Box<TargetResourceCommand>),
}
pub struct TargetResourceCommand {
    state: Weak<Mutex<State>>,
    cell: Weak<Mutex<Cell>>,
    installed: Weak<Installed>,
    kernel: Arc<ReservationFamilyExclusive>,
    caller: slot::Caller,
    process: Process,
    report: Report,
    source: Arc<slot::SourceHandle>,
    connection: Connection,
    work: Arc<Work>,
}
impl std::fmt::Debug for TargetResourceCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TargetResourceCommand(redacted)")
    }
}

fn receipt(record: &Record) -> Result<TargetResourceReceipt, Error> {
    if record.revoked || record.refused {
        return Err(Error::Stale);
    }
    Ok(TargetResourceReceipt {
        token: record.receipt.clone().ok_or(Error::Busy)?,
    })
}

/// Validation-only authority, distinct from the constructor read that may
/// materialize B. Only the resource worker creates this borrowed proof.
pub(in crate::harness) struct TargetSnapshotRead<'a> {
    command: &'a TargetResourceCommand,
    installed: &'a Arc<Installed>,
}
impl TargetSnapshotRead<'_> {
    pub(in crate::harness) fn caller(&self) -> &slot::Caller {
        &self.command.caller
    }
    pub(in crate::harness) fn assignment(&self) -> &assignment::AssignmentCommit {
        &self.installed.assignment
    }
    pub(in crate::harness) fn expected(&self) -> &slot::SourceHandle {
        &self.command.source
    }
    pub(in crate::harness) fn gate(&self) -> &SourceGateIdentity {
        self.installed.resource.gate_identity()
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        self.command.kernel.current().map_err(str::to_owned)?;
        self.installed.resource.current().map_err(str::to_owned)?;
        if self.command.work.scope.cancelled()
            || !self.command.connection.is_live()
            || Process::observe(self.command.caller.pid).as_ref() != Some(&self.command.process)
        {
            return Err("target resource owner changed".into());
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
            .ok_or("target installation retired")?;
        let state = lock(&state).map_err(|_| "target registry busy")?;
        same_cell(&state, &cell, self.caller()).map_err(|_| "target reservation changed")?;
        let held = lock(&cell).map_err(|_| "target reservation busy")?;
        if !matches!(&held.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, &self.command.kernel))
        {
            return Err("target exclusion changed".into());
        }
        check_origin(
            &state,
            &held,
            self.installed,
            &self.command.process,
            &self.command.connection,
            &self.command.report,
        )
        .map_err(|_| "target resources changed")?;
        let record = state
            .target_resources
            .get(&open_key(self.caller(), &self.command.report.operation))
            .ok_or("target resource attempt missing")?;
        if record.refused
            || record.revoked
            || record
                .pending
                .as_ref()
                .is_none_or(|work| !Arc::ptr_eq(work, &self.command.work))
        {
            return Err("target resource attempt changed".into());
        }
        Ok(())
    }
}

fn check_origin(
    state: &State,
    cell: &Cell,
    installed: &Arc<Installed>,
    process: &Process,
    connection: &Connection,
    report: &Report,
) -> Result<(), Error> {
    if cell.process != *process
        || !matches!(&cell.phase, seal::Phase::Sealed { .. })
        || !cell
            .installed
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, installed))
        || !cell.target_constructors.is_empty()
        || cell
            .target_retiring
            .iter()
            .any(|witness| !witness.retired())
        || installed.boundary != report.boundary
        || installed.assignment.generation() != report.assignment_generation
        || !allows_member(state, cell, installed.assignment.caller(), &report.member)
    {
        return Err(Error::Stale);
    }
    let actor = state
        .actors
        .get(&actor_key(installed.assignment.caller()))
        .ok_or(Error::Stale)?;
    if actor.process != *process {
        return Err(Error::Stale);
    }
    for member in cell
        .target_issued
        .keys()
        .filter(|member| actor.issued.contains_key(*member))
    {
        let native_member_key = member_key(installed.assignment.caller(), member)?;
        if state.preloads.contains_key(&native_member_key)
            || state
                .preload_sequences
                .get(&native_member_key)
                .is_some_and(|ledger| !ledger.retired())
        {
            return Err(Error::Busy);
        }
        if state.unrelated.contains_key(&native_member_key) {
            super::no_reader::observed(
                state,
                installed.assignment.caller(),
                process,
                member,
                installed,
            )
            .ok_or(Error::Stale)?;
            continue;
        }
        let loaded = state
            .members
            .get(&member_key(installed.assignment.caller(), member)?)
            .ok_or(Error::Stale)?;
        if loaded.unknown
            || loaded.process != *process
            || !loaded.connection.is_live()
            || !same_source_scope(&loaded.source, &installed.source)
            || !revision_descends(
                loaded.source.credential_generation(),
                loaded.source.digest(),
                installed.source.credential_generation(),
                installed.source.digest(),
            )
        {
            return Err(Error::Stale);
        }
        match loaded.readiness.role {
            Some(ReadyRole::Manager(_)) => {}
            Some(ReadyRole::ReaderLoaded)
                if loaded.readiness.reader_owned
                    && matches!(&loaded.readiness.gate, Some(ReaderGate::Target(gate)) if Arc::ptr_eq(gate, &installed.resource)) =>
                {}
            _ => return Err(Error::Stale),
        }
    }
    let member = state
        .members
        .get(&member_key(installed.assignment.caller(), &report.member)?)
        .ok_or(Error::Stale)?;
    if member.unknown
        || !member.connection.same(connection)
        || member.registration != report.registration
        || !report.matches_source(&member.source)
        || member.readiness.role != Some(ReadyRole::Manager(report.auth))
    {
        return Err(Error::Stale);
    }
    Ok(())
}

pub(super) fn authenticates_applied(
    state: &State,
    caller: &slot::Caller,
    applied: &Arc<assignment::target_ack::AppliedReceipt>,
) -> bool {
    state
        .target_resources
        .get(&open_key(caller, &applied.observation().resource_operation))
        .is_some_and(|record| {
            record.caller == *caller
                && record
                    .applied
                    .as_ref()
                    .is_some_and(|actual| Arc::ptr_eq(actual, applied))
        })
}

pub(super) fn normalization_progress(
    state: &State,
    caller: &slot::Caller,
    applied: &Arc<assignment::target_ack::AppliedReceipt>,
) -> Option<Arc<super::normalize::NormalizationRetirement>> {
    state
        .target_resources
        .get(&open_key(caller, &applied.observation().resource_operation))
        .filter(|record| {
            record
                .applied
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, applied))
        })
        .and_then(|record| record.normalized.as_ref())
        .cloned()
}
pub(super) fn record_normalization(
    state: &mut State,
    caller: &slot::Caller,
    applied: &Arc<assignment::target_ack::AppliedReceipt>,
    proof: Arc<super::normalize::NormalizationRetirement>,
) -> Result<(), String> {
    let record = state
        .target_resources
        .get_mut(&open_key(caller, &applied.observation().resource_operation))
        .ok_or("target ACK record disappeared")?;
    if record
        .applied
        .as_ref()
        .is_none_or(|actual| !Arc::ptr_eq(actual, applied))
        || record.normalized.is_some()
    {
        return Err("target normalization changed".into());
    }
    record.normalized = Some(proof);
    Ok(())
}

pub(super) fn ready_to_release(
    state: &State,
    caller: &slot::Caller,
    applied: &Arc<assignment::target_ack::AppliedReceipt>,
) -> bool {
    state
        .target_resources
        .get(&open_key(caller, &applied.observation().resource_operation))
        .is_some_and(|record| {
            record
                .applied
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, applied))
                && record.cancellation.retired()
                && record.ack.is_none()
                && record
                    .ack_retirement
                    .as_ref()
                    .is_some_and(|retired| retired.retired())
        })
}

pub(in crate::harness) struct ResourceRetirement {
    resources: Vec<Arc<Work>>,
    acks: Vec<Arc<AckWork>>,
    normalizations: Vec<Arc<super::normalize::NormalizationRetirement>>,
}
impl ResourceRetirement {
    pub(in crate::harness) fn len(&self) -> usize {
        self.resources.len() + self.acks.len()
    }
}
impl Drop for ResourceRetirement {
    fn drop(&mut self) {
        self.resources.clear();
        self.acks.clear();
        for proof in self.normalizations.drain(..) {
            proof.abandon_departed();
        }
    }
}
pub(in crate::harness) fn owners(state: &State) -> Vec<(slot::Caller, Process)> {
    state
        .target_resources
        .values()
        .map(|record| (record.caller.clone(), record.process.clone()))
        .collect()
}

/// Called only by the exact kernel-departure sweep, never socket EOF. Removing
/// map custody grants no terminal result: real command captures still own their
/// counted operations and physical resources until they actually retire.
pub(in crate::harness) fn revoke_departed(
    state: &mut State,
    caller: &slot::Caller,
    expected: &Process,
) -> ResourceRetirement {
    let mut resources = Vec::new();
    let mut acks = Vec::new();
    let mut normalizations = Vec::new();
    for record in state
        .target_resources
        .values_mut()
        .filter(|record| record.caller == *caller && record.process == *expected)
    {
        if record.applied.is_none() {
            record.revoked = true;
        }
        if let Some(proof) = &record.normalized {
            normalizations.push(proof.clone());
        }
        record.cancellation.cancel();
        if let Some(work) = record.pending.take() {
            resources.push(work);
        }
        if let Some(work) = record.ack.take() {
            work.scope.cancellation().cancel();
            acks.push(work);
        }
    }
    ResourceRetirement {
        resources,
        acks,
        normalizations,
    }
}
pub(in crate::harness) fn custody_pending(state: &State, caller: &slot::Caller) -> bool {
    state.target_resources.values().any(|record| {
        record.caller == *caller
            && (!record.cancellation.retired()
                || record
                    .ack_retirement
                    .as_ref()
                    .is_some_and(|witness| !witness.retired()))
    })
}

/// Only an already authenticated explicit member retirement calls this. EOF
/// deliberately does not revoke an issued resource observation.
pub(in crate::harness) fn revoke_member(
    state: &mut State,
    caller: &slot::Caller,
    member: &str,
) -> ResourceRetirement {
    let mut retired = Vec::new();
    for record in state.target_resources.values_mut().filter(|record| {
        record.caller == *caller
            && (record.report.member == member
                || record.cohort.iter().any(|entry| entry.member() == member))
    }) {
        if record.applied.is_none() {
            record.revoked = true;
        }
        if let Some(work) = record.pending.take() {
            retired.push(work);
        }
    }
    ResourceRetirement {
        resources: retired,
        acks: Vec::new(),
        normalizations: Vec::new(),
    }
}

/// Serialize destination metadata changes with the durable ACK attempt. Already
/// sent provider work may retain raw completion, but cannot replace the loaded
/// observation while its signed ACK is being published.
pub(in crate::harness) fn check_member_update(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> Result<(), Error> {
    if state.target_resources.values().any(|record| {
        record.caller == *caller
            && record.ack.is_some()
            && (record.report.member == member
                || record.cohort.iter().any(|entry| entry.member() == member))
    }) {
        return Err(Error::Busy);
    }
    Ok(())
}

pub(in crate::harness) fn check_retirement(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> Result<(), Error> {
    if state
        .target_resources
        .values()
        .filter(|record| {
            record.caller == *caller
                && (record.report.member == member
                    || record.cohort.iter().any(|entry| entry.member() == member))
        })
        .any(|record| {
            record.ack.is_some()
                || record.pending.as_ref().is_some_and(|work| {
                    work.worker.load(Ordering::Acquire) || Arc::strong_count(work) != 1
                })
        })
    {
        return Err(Error::Busy);
    }
    Ok(())
}

pub(super) fn member_captures_retired(state: &State, caller: &slot::Caller, member: &str) -> bool {
    state
        .target_resources
        .values()
        .filter(|record| {
            record.caller == *caller
                && (record.report.member == member
                    || record.cohort.iter().any(|entry| entry.member() == member))
        })
        .all(|record| record.ack.is_none() && record.work_identity.upgrade().is_none())
}

pub(in crate::harness) fn refuse_cancelled(state: &mut State) -> ResourceRetirement {
    let mut retired = Vec::new();
    for record in state.target_resources.values_mut() {
        if record
            .pending
            .as_ref()
            .is_some_and(|work| !work.worker.load(Ordering::Acquire) && work.scope.cancelled())
        {
            record.refused = true;
            if let Some(work) = record.pending.take() {
                retired.push(work);
            }
        }
    }
    // A never-dispatched ACK is validation-only. Fence its exact accepted
    // record before releasing the map's Scope; captured commands remain counted
    // and cannot arm once the retained identity is removed.
    let mut acks = Vec::new();
    let keys: Vec<_> = state
        .target_resources
        .iter()
        .filter(|(_, record)| {
            record.ack.as_ref().is_some_and(|work| {
                work.scope.cancelled()
                    && !work.worker.load(Ordering::Acquire)
                    && !work.attempted.load(Ordering::Acquire)
            })
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        let Some(record) = state.target_resources.get(&key) else {
            continue;
        };
        let caller = record.caller.clone();
        let operation = record.report.operation.clone();
        let cell = state.reservations.get(&actor_key(&caller)).cloned();
        let Some(cell) = cell else { continue };
        let Ok(mut held) = lock(&cell) else { continue };
        if !matches!(&held.resource_phase, Phase::Acknowledging { operation: current } if current == &operation)
        {
            continue;
        }
        if let Some(work) = state
            .target_resources
            .get_mut(&key)
            .and_then(|record| record.ack.take())
        {
            acks.push(work);
            held.resource_phase = Phase::Ready { operation };
        }
    }
    ResourceRetirement {
        resources: retired,
        acks,
        normalizations: Vec::new(),
    }
}

fn arm_work(
    state: &State,
    caller: &slot::Caller,
    report: &Report,
    process: &Process,
    source: &Arc<slot::SourceHandle>,
    work: &Arc<Work>,
) -> Result<(), Error> {
    let record = state
        .target_resources
        .get(&open_key(caller, &report.operation))
        .ok_or(Error::Stale)?;
    if record.caller != *caller
        || record.process != *process
        || record.report != *report
        || record.refused
        || record.revoked
        || record.cancellation.cancelled()
        || work.scope.cancelled()
        || !Arc::ptr_eq(&record.source, source)
        || record
            .pending
            .as_ref()
            .is_none_or(|pending| !Arc::ptr_eq(pending, work))
        || !record.work_identity.ptr_eq(&Arc::downgrade(work))
        || state
            .app_lifetime
            .as_ref()
            .is_none_or(|app| !work.scope.cancellation().owned_by(app))
    {
        return Err(Error::Stale);
    }
    work.worker
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Error::Busy)?;
    Ok(())
}

fn refuse_work(
    state: &mut State,
    caller: &slot::Caller,
    report: &Report,
    process: &Process,
    work: &Arc<Work>,
) -> Result<Option<Arc<Work>>, Error> {
    let record = state
        .target_resources
        .get_mut(&open_key(caller, &report.operation))
        .ok_or(Error::Stale)?;
    if record.report != *report
        || record.process != *process
        || !record.work_identity.ptr_eq(&Arc::downgrade(work))
        || record.receipt.is_some()
    {
        return Err(Error::Stale);
    }
    if work.worker.load(Ordering::Acquire) {
        return Err(Error::Busy);
    }
    record.refused = true;
    Ok(record.pending.take())
}

impl Registry {
    /// Issued-receipt replay is correlation only and deliberately precedes
    /// foreground/connection/Ready admission. Unknown attempts mint nothing.
    pub fn replay_target_resources(
        &self,
        caller: &slot::Caller,
        assignment: &str,
        context: &str,
        report: ResourceReport<'_>,
    ) -> Result<Option<TargetResourceReceipt>, Error> {
        if !hex(assignment, 32) || !hex(context, 64) {
            return Err(Error::Denied);
        }
        let report = Report::parse(report)?;
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        let state = lock(&self.state)?;
        let Some(record) = state
            .target_resources
            .get(&open_key(caller, &report.operation))
        else {
            return Ok(None);
        };
        if record.caller != *caller
            || record.process != process
            || record.assignment != assignment
            || record.context != context
            || record.report != report
            || !record.report.matches_source(&record.source)
        {
            return Err(Error::Stale);
        }
        if record.receipt.is_some() {
            return receipt(record).map(Some);
        }
        if record.refused || record.revoked {
            return Err(Error::Stale);
        }
        Ok(None)
    }

    pub fn prepare_target_resources(
        &self,
        peer: &Peer,
        connection: &Connection,
        role: &InstalledTarget,
        report: ResourceReport<'_>,
    ) -> Result<ResourcePreparation, Error> {
        let report = Report::parse(report)?;
        if !peer.current() || !connection.is_live() {
            return Err(Error::Denied);
        }
        let installed = role.installed.upgrade().ok_or(Error::Stale)?;
        let cell = role.cell.upgrade().ok_or(Error::Stale)?;
        if role
            .state
            .upgrade()
            .is_none_or(|state| !Arc::ptr_eq(&state, &self.state))
        {
            return Err(Error::Stale);
        }
        let mut state = lock(&self.state)?;
        same_cell(&state, &cell, &peer.caller)?;
        let mut held = lock(&cell)?;
        let kernel = match &held.phase {
            seal::Phase::Sealed { kernel, .. } => kernel.clone(),
            _ => return Err(Error::Stale),
        };
        if let Phase::Ready { operation } = &held.resource_phase {
            let prior_key = open_key(&peer.caller, operation);
            if state.target_resources.get(&prior_key).is_some_and(|prior| {
                prior.applied.is_none()
                    && (prior.revoked || !cohort_current(&state, &peer.caller, &prior.cohort))
            }) {
                if let Some(prior) = state.target_resources.get_mut(&prior_key) {
                    prior.revoked = true;
                }
                held.resource_phase = Phase::Initializing;
            }
        }
        let key = open_key(&peer.caller, &report.operation);
        let (work, source, original_connection) = if let Some(record) =
            state.target_resources.get(&key)
        {
            if record.report != report
                || record.context != installed.context
                || record.assignment != installed.assignment.operation()
                || record.process != peer.process
            {
                return Err(Error::Stale);
            }
            if record.receipt.is_some() {
                return receipt(record).map(ResourcePreparation::Ready);
            }
            if record.refused
                || record.revoked
                || !matches!(&held.resource_phase, Phase::Preparing { operation } if operation == &report.operation)
            {
                return Err(Error::Stale);
            }
            // Resuming exact accepted validation is not new source admission.
            // A departed original connection makes the worker refuse, not rebind.
            (
                record.pending.clone().ok_or(Error::Stale)?,
                record.source.clone(),
                record.connection.clone(),
            )
        } else {
            if !held.resource_phase.permits_factory() {
                return Err(Error::Busy);
            }
            check_origin(
                &state,
                &held,
                &installed,
                &peer.process,
                connection,
                &report,
            )?;
            if state.target_resources.len() >= MAX_ACTORS * MAX_MEMBERS
                || state
                    .target_resources
                    .values()
                    .filter(|record| record.caller == peer.caller)
                    .count()
                    >= MAX_MEMBERS
            {
                return Err(Error::Busy);
            }
            reservation_cell_scope(&state, &held)?;
            let scope = state
                .app_lifetime
                .as_ref()
                .and_then(|app| app.enter())
                .ok_or(Error::Busy)?;
            let work = Arc::new(Work {
                scope,
                worker: false.into(),
            });
            let source = state
                .members
                .get(&member_key(&peer.caller, &report.member)?)
                .ok_or(Error::Stale)?
                .source
                .clone();
            state.target_resources.insert(
                key,
                Record {
                    caller: peer.caller.clone(),
                    process: peer.process.clone(),
                    assignment: installed.assignment.operation().into(),
                    context: installed.context.clone(),
                    report: report.clone(),
                    source: source.clone(),
                    connection: connection.clone(),
                    pending: Some(work.clone()),
                    work_identity: Arc::downgrade(&work),
                    receipt: None,
                    cohort: Vec::new(),
                    ack: None,
                    ack_retirement: None,
                    applied: None,
                    normalized: None,
                    refused: false,
                    revoked: false,
                    cancellation: work.scope.cancellation(),
                },
            );
            // Close destination construction before the host may expose Waiting.
            held.resource_phase = Phase::Preparing {
                operation: report.operation.clone(),
            };
            (work, source, connection.clone())
        };
        Ok(ResourcePreparation::Pending(Box::new(
            TargetResourceCommand {
                state: Arc::downgrade(&self.state),
                cell: Arc::downgrade(&cell),
                installed: Arc::downgrade(&installed),
                kernel,
                caller: peer.caller.clone(),
                process: peer.process.clone(),
                report,
                source,
                connection: original_connection,
                work,
            },
        )))
    }

    pub fn run_target_resources(
        &self,
        command: Box<TargetResourceCommand>,
        env: &Env,
    ) -> Result<TargetResourceReceipt, Error> {
        {
            let state = command.state.upgrade().ok_or(Error::Stale)?;
            if !Arc::ptr_eq(&state, &self.state) {
                return Err(Error::Stale);
            }
            let state = lock(&state)?;
            // This exact accepted-work fence precedes even physical probes.
            arm_work(
                &state,
                &command.caller,
                &command.report,
                &command.process,
                &command.source,
                &command.work,
            )?;
        }
        struct Worker(Arc<Work>);
        impl Drop for Worker {
            fn drop(&mut self) {
                self.0.worker.store(false, Ordering::Release);
            }
        }
        let _worker = Worker(command.work.clone());
        let installed = command.installed.upgrade();
        let mut validated = if let Some(installed) = &installed {
            let permission = TargetSnapshotRead {
                command: &command,
                installed,
            };
            slot::validate_target_snapshot(env, &permission).map_err(|_| Error::Stale)
        } else {
            Err(Error::Stale)
        };
        let token = if validated.is_ok() {
            match aterm_uds::rand::hex_token::<32>() {
                Ok(token) => Some(token),
                Err(_) => {
                    validated = Err(Error::Unavailable);
                    None
                }
            }
        } else {
            None
        };
        let (result, retired) = {
            let mut state = lock(&self.state)?;
            let cell = command.cell.upgrade();
            let mut held = match cell.as_ref() {
                Some(cell) => Some(lock(cell)?),
                None => None,
            };
            let fresh = if let (Some(held), Some(installed)) = (held.as_ref(), installed.as_ref()) {
                !command.work.scope.cancelled()
                    && command.connection.is_live()
                    && cell
                        .as_ref()
                        .is_some_and(|cell| same_cell(&state, cell, &command.caller).is_ok())
                    && reservation_cell_scope(&state, held).is_ok()
                    && matches!(&held.phase, seal::Phase::Sealed { kernel, .. } if Arc::ptr_eq(kernel, &command.kernel))
                    && matches!(&held.resource_phase, Phase::Preparing { operation } if operation == &command.report.operation)
                    && check_origin(
                        &state,
                        held,
                        installed,
                        &command.process,
                        &command.connection,
                        &command.report,
                    )
                    .is_ok()
            } else {
                false
            };
            if !fresh {
                validated = Err(Error::Stale);
            }
            let observed = if validated.is_ok() {
                match held
                    .as_ref()
                    .map(|held| cohort(&state, held, &command.caller))
                {
                    Some(Ok(observed)) => observed,
                    _ => {
                        validated = Err(Error::Stale);
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let key = open_key(&command.caller, &command.report.operation);
            let record = state.target_resources.get_mut(&key).ok_or(Error::Stale)?;
            if record.report != command.report
                || record
                    .pending
                    .as_ref()
                    .is_none_or(|work| !Arc::ptr_eq(work, &command.work))
            {
                return Err(Error::Stale);
            }
            record.cohort = observed;
            record.refused = validated.is_err();
            record.receipt = if validated.is_ok() { token } else { None };
            let retired = record.pending.take();
            let result = validated.and_then(|()| receipt(record));
            if let Some(held) = held.as_mut() {
                finish_phase(held, &command.report.operation, result.is_ok());
            }
            (result, retired)
        };
        drop(retired);
        result
    }

    /// Host-side failed-dispatch settlement. The exact command is an owned
    /// no-effect capability; it cannot revoke an already issued receipt.
    pub fn refuse_target_resources(&self, command: &TargetResourceCommand) -> Result<(), Error> {
        let state = command.state.upgrade().ok_or(Error::Stale)?;
        if !Arc::ptr_eq(&state, &self.state) {
            return Err(Error::Stale);
        }
        let retired = {
            let mut state = lock(&state)?;
            let cell = state.reservations.get(&actor_key(&command.caller)).cloned();
            let mut held = match cell.as_ref() {
                Some(cell) => Some(lock(cell)?),
                None => None,
            };
            let retired = refuse_work(
                &mut state,
                &command.caller,
                &command.report,
                &command.process,
                &command.work,
            )?;
            if let Some(held) = held.as_mut() {
                finish_phase(held, &command.report.operation, false);
            }
            retired
        };
        drop(retired);
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "codex_source_target_resource_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
pub(in crate::harness::codex_source_registry) fn pending_capture_fixture(
    registry: &Registry,
    caller: &slot::Caller,
    process: &Process,
    connection: &Connection,
    source: Arc<slot::SourceHandle>,
) -> impl Send {
    // Lifecycle-only injection: no InstalledTarget, ResourceReceipt, supported
    // image, source seal, owner grant or ACK proof can be constructed from it.
    let app = lock(&registry.state).unwrap().app_lifetime.clone().unwrap();
    let work = Arc::new(Work {
        scope: app.enter().unwrap(),
        worker: false.into(),
    });
    let report = Report {
        operation: "1".repeat(32),
        member: "2".repeat(32),
        registration: "3".repeat(32),
        boundary: "4".repeat(32),
        assignment_generation: source.assignment_generation(),
        credential_generation: source.credential_generation(),
        digest: source.digest().into(),
        account: source.account().into(),
        user: source.user().into(),
        auth: AuthState {
            generation: 1,
            owner: 1,
            source: 1,
            login: 1,
        },
    };
    lock(&registry.state).unwrap().target_resources.insert(
        open_key(caller, &report.operation),
        Record {
            caller: caller.clone(),
            process: process.clone(),
            assignment: "5".repeat(32),
            context: "6".repeat(64),
            report,
            source,
            connection: connection.clone(),
            pending: Some(work.clone()),
            work_identity: Arc::downgrade(&work),
            receipt: None,
            cohort: Vec::new(),
            ack: None,
            ack_retirement: None,
            applied: None,
            normalized: None,
            refused: false,
            revoked: false,
            cancellation: work.scope.cancellation(),
        },
    );
    work
}

/// A removed pending map entry is not proof that an external native capture
/// retired. This join is used before closing a completed NoReader OPEN.
pub(super) fn check_retired_member_captures(
    state: &State,
    caller: &slot::Caller,
    member: &str,
) -> Result<(), Error> {
    if state
        .target_resources
        .values()
        .filter(|record| {
            record.caller == *caller
                && (record.report.member == member
                    || record.cohort.iter().any(|entry| entry.member() == member))
        })
        .any(|record| {
            (record.pending.is_none() && !record.cancellation.retired())
                || record
                    .ack_retirement
                    .as_ref()
                    .is_some_and(|witness| !witness.retired())
        })
    {
        return Err(Error::Busy);
    }
    Ok(())
}
