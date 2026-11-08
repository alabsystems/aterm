// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original cooperative workload custody. The primary is a real canonical
//! lease acquired before expensive recipe preparation; a ceiling or readiness
//! frame can never construct it. Foreign phases require a separate transition.

use super::*;
use std::{
    os::fd::{AsFd as _, AsRawFd as _},
    os::unix::ffi::OsStrExt as _,
    sync::{Arc, Mutex},
};
use trust_resource_controller::workload::{self as wire, host};

#[path = "resource_admission_workload_bank.rs"]
mod bank;
#[path = "resource_admission_workload_output.rs"]
mod output;
pub(super) use output::OutputLocks;
#[path = "resource_admission_workload_phase.rs"]
mod phase;
#[path = "resource_admission_workload_produced.rs"]
mod produced;
#[path = "resource_admission_workload_census.rs"]
mod snapshot;
pub(crate) use bank::{WorkerCustody, WorkerOffer, WorkerReturn};
pub(super) use produced::Member as ProducedMember;
pub(crate) use produced::RegistrationPending;

pub(super) const PRIMARY_TARGET: i32 = 6;

pub(super) fn compiler_input(
    sdk: &sdk::Keeper,
    root_deadline: Instant,
    started: Instant,
) -> io::Result<(
    Arc<trust_resource_provenance::program::HeldProgram>,
    Instant,
)> {
    let deadline = root_deadline.min(native_inputs::deadline(started));
    Ok((sdk.workload_compiler(deadline)?, deadline))
}

pub(super) fn purpose(request: &aterm_types::resource_admission::Submission) -> bool {
    request
        .argv
        .get(1)
        .is_some_and(|word| word == wire::WORKLOAD_STARTUP_ARGUMENT.as_bytes())
}
pub(super) fn closed_request(request: &aterm_types::resource_admission::Submission) -> bool {
    request.cpu_units > 0
        && request.timeout_ms.is_some_and(|ms| {
            ms > 0
                && ms <= host::MAX_ROOT_TIMEOUT_MS
                && Instant::now()
                    .checked_add(Duration::from_millis(ms))
                    .is_some()
        })
        && matches!(request.argv.as_slice(), [_, entry] if entry == wire::WORKLOAD_STARTUP_ARGUMENT.as_bytes())
}

pub(super) struct Primary {
    reservation: Mutex<aterm_capacity::build_cpu::NativeDomainReservation>,
}
impl Primary {
    pub(super) fn new(
        reservation: aterm_capacity::build_cpu::NativeDomainReservation,
    ) -> Arc<Self> {
        Arc::new(Self {
            reservation: Mutex::new(reservation),
        })
    }
    pub(super) fn configure(
        &self,
        command: &mut spawnfd::Command,
        sdk: &sdk::Keeper,
    ) -> io::Result<()> {
        if command.guard_targets().len() != 0 {
            return Err(refused("workload primary cannot replace inherited custody"));
        }
        let reservation = self
            .reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?;
        let set = reservation.native_leases();
        if set.is_empty() || set.len() > host::MAX_CPU_SET {
            return Err(refused("unsupported original CPU set"));
        }
        let mut guards = vec![
            (sdk.descriptor()?, sdk::TARGET),
            (crate::runtime::workload_store_description()?, 3),
        ];
        let mut targets = Vec::with_capacity(set.len());
        for (index, lease) in set.iter().enumerate() {
            let target = PRIMARY_TARGET + index as i32;
            guards.push((
                aterm_uds::kept_out(|| lease.as_fd().try_clone_to_owned())?,
                target,
            ));
            targets.push(target.to_string());
        }
        command.inherit_guards_at(guards)?;
        command.env(host::CPU_SET_FDS_ENV, targets.join(","));
        command.env(host::HOST_STORE_FD_ENV, "3");
        command.env("ATERM_SDK_LEASE_V3", sdk::TARGET.to_string());
        Ok(())
    }
    pub(super) fn bind_child(&self, pid: u32) -> io::Result<()> {
        let owner = aterm_capacity::build_cpu::process_cpu::read(pid)
            .ok_or_else(|| refused("original workload CPU child identity unavailable"))?
            .owner;
        let reservation = self
            .reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?;
        for lease in reservation.native_leases() {
            lease.report_owner(owner)?;
        }
        Ok(())
    }
    pub(super) fn observations(&self) -> io::Result<Vec<host::PrimaryCpu>> {
        use std::os::unix::fs::MetadataExt as _;
        let reservation = self
            .reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?;
        reservation
            .native_leases()
            .iter()
            .enumerate()
            .map(|(index, lease)| {
                let metadata = spawnfd::descriptor_metadata(lease.as_raw_fd())?;
                Ok(host::PrimaryCpu {
                    inherited_fd: PRIMARY_TARGET as u32 + index as u32,
                    slot: lease.slot(),
                    backing: trust_resource_controller::FileIdentity {
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    },
                })
            })
            .collect()
    }
    pub(super) fn descriptions(&self) -> io::Result<Vec<std::os::fd::OwnedFd>> {
        let reservation = self
            .reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?;
        reservation
            .native_leases()
            .iter()
            .map(|lease| aterm_uds::kept_out(|| lease.as_fd().try_clone_to_owned()))
            .collect()
    }
    pub(super) fn settle(&self) -> io::Result<()> {
        self.reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?
            .settle_exclusion()
    }
    pub(super) fn original_for(
        &self,
        offered: std::os::fd::BorrowedFd<'_>,
    ) -> io::Result<aterm_capacity::build_cpu::Lease> {
        use std::os::unix::fs::MetadataExt as _;
        let observed = spawnfd::descriptor_metadata(offered.as_raw_fd())?;
        let reservation = self
            .reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?;
        for lease in reservation.native_leases() {
            let held = spawnfd::descriptor_metadata(lease.as_raw_fd())?;
            if held.dev() == observed.dev() && held.ino() == observed.ino() {
                // This clone comes from the original typed reservation, never
                // the caller-offered independently opened description.
                return lease.try_clone();
            }
        }
        Err(refused(
            "offered CPU does not name this workload's original set",
        ))
    }
    fn original_slot(&self, slot: u32) -> io::Result<aterm_capacity::build_cpu::Lease> {
        let reservation = self
            .reservation
            .lock()
            .map_err(|_| refused("workload reservation poisoned"))?;
        reservation
            .native_leases()
            .iter()
            .find(|lease| lease.slot() == slot)
            .ok_or_else(|| refused("registered member has no original workload guard"))?
            .try_clone()
    }
}

pub(crate) struct ReadyCustody {
    pub(crate) reply: host::ReadyReply,
    owner: Arc<Primary>,
    deadline: Instant,
}
impl ReadyCustody {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn descriptions(&self) -> io::Result<Vec<std::os::fd::OwnedFd>> {
        self.owner.descriptions()
    }
}
pub(crate) struct MemberOriginalCustody {
    pub(crate) reply: host::MemberOriginalReply,
    original: std::os::fd::OwnedFd,
    _owner: Arc<Primary>,
    _sdk: Arc<sdk::Keeper>,
}
impl MemberOriginalCustody {
    pub(crate) fn description(&self) -> std::os::fd::BorrowedFd<'_> {
        self.original.as_fd()
    }
}

pub(super) struct State {
    pub(super) primary: Arc<Primary>,
    cwd: Vec<u8>,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    ready: Option<Ready>,
    pub(super) queries: usize,
    pub(super) census: BTreeMap<[u8; 16], Census>,
    member_originals: std::collections::BTreeSet<[u8; 16]>,
    pub(super) control: Option<crate::workload_transport::Link>,
    bank: bank::Loans,
    phase: phase::Phases,
    snapshot: snapshot::Snapshots,
    produced: produced::Outputs,
    output: Option<Arc<output::OutputLocks>>,
    output_attempted: bool,
}
#[derive(Clone, Copy)]
pub(super) enum CensusKind {
    Compiler,
    Linker,
    Query,
    BuildScript,
    Inventory,
    Sample,
}
#[derive(Clone)]
pub(super) struct Census {
    pub(super) kind: CensusKind,
    pub(super) identity: Option<(u32, u64)>,
    pub(super) completion: Option<member::Completed>,
    pub(super) started: Option<member::Started>,
    pub(super) retired: bool,
    pub(super) drained: bool,
    pub(super) uncertain: bool,
    pub(super) closed: bool,
    pub(super) refused: bool,
}
struct Ready {
    invocation: wire::WorkloadInvocation,
    sha256: [u8; 32],
    root_attempt: [u8; 16],
    // These are opaque live original owners, not reconstructed observations.
    _output: Arc<trust_resource_provenance::program::produced::HeldOutputRoot>,
}
impl State {
    pub(super) fn registration_preparing(&self) -> bool {
        self.produced.preparing()
    }
    pub(super) fn foreign_member(&self, attempt: [u8; 16]) -> bool {
        self.census.get(&attempt).is_some_and(|entry| {
            matches!(
                entry.kind,
                CensusKind::BuildScript | CensusKind::Inventory | CensusKind::Sample
            )
        })
    }
    pub(super) fn new(primary: Arc<Primary>, recipe: &recipe::Recipe, cwd: Vec<u8>) -> Self {
        let environment = recipe
            .environment
            .iter()
            .filter(|(name, _)| {
                matches!(
                    name.as_bytes(),
                    b"LANG"
                        | b"LC_ALL"
                        | b"CARGO"
                        | b"RUSTC"
                        | b"CARGO_HOME"
                        | b"CARGO_CACHE_RUSTC_INFO"
                )
            })
            .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        Self {
            primary,
            cwd,
            environment,
            ready: None,
            queries: 0,
            census: BTreeMap::new(),
            member_originals: std::collections::BTreeSet::new(),
            control: None,
            bank: bank::Loans::default(),
            phase: phase::Phases::default(),
            snapshot: snapshot::Snapshots::default(),
            produced: produced::Outputs::default(),
            output: None,
            output_attempted: false,
        }
    }
    pub(super) fn register(&mut self, attempt: [u8; 16], kind: CensusKind) -> io::Result<()> {
        if self.census.len() == 4096 || self.census.contains_key(&attempt) {
            return Err(refused("workload member census is incomplete or replayed"));
        }
        self.census.insert(
            attempt,
            Census {
                kind,
                identity: None,
                completion: None,
                started: None,
                retired: false,
                drained: false,
                uncertain: false,
                closed: false,
                refused: false,
            },
        );
        Ok(())
    }
    pub(super) fn observe(
        &mut self,
        attempt: [u8; 16],
        identity: Option<(u32, u64)>,
        progress: &member::Progress,
    ) -> io::Result<()> {
        let entry = self
            .census
            .get_mut(&attempt)
            .ok_or_else(|| refused("workload member omitted from original census"))?;
        if let Some(identity) = identity {
            if entry.identity.is_some_and(|before| before != identity) {
                return Err(refused("workload member identity changed"));
            }
            entry.identity = Some(identity);
        }
        if let Some(completion) = &progress.completed {
            entry.completion = Some(completion.clone());
        }
        // Reaping/outcome alone does not establish original resource return.
        entry.retired |= progress.retired;
        entry.drained |= progress.drained;
        entry.uncertain |= progress.uncertain;
        entry.closed |= entry.retired && entry.drained;
        Ok(())
    }
    pub(super) fn refused(&mut self, attempt: [u8; 16]) -> io::Result<()> {
        let entry = self
            .census
            .get_mut(&attempt)
            .ok_or_else(|| refused("refused workload member omitted from census"))?;
        if entry.identity.is_some() || entry.completion.is_some() {
            return Err(refused(
                "executed member cannot become a no-execution refusal",
            ));
        }
        entry.refused = true;
        entry.closed = true;
        entry.retired = true;
        entry.drained = true;
        Ok(())
    }
    pub(super) fn started(
        &mut self,
        attempt: [u8; 16],
        started: member::Started,
    ) -> io::Result<()> {
        let entry = self
            .census
            .get_mut(&attempt)
            .ok_or_else(|| refused("started workload member omitted from original census"))?;
        if entry.started.is_some() || entry.refused || entry.completion.is_some() {
            return Err(refused(
                "workload member started observation replayed or reordered",
            ));
        }
        entry.started = Some(started);
        Ok(())
    }
    pub(super) fn compiler_dispatch(&self) -> bool {
        self.output.is_some() && self.cooperative_dispatch()
    }
    pub(super) fn retained_output_locks(&self) -> Option<Arc<OutputLocks>> {
        self.output.clone()
    }
    fn cooperative_dispatch(&self) -> bool {
        self.ready.is_some()
            && self.phase.cooperative()
            && self.control.as_ref().is_some_and(|link| !link.closed())
    }
    fn ready_digest(&self) -> io::Result<[u8; 32]> {
        self.ready
            .as_ref()
            .map(|ready| ready.sha256)
            .ok_or_else(|| refused("workload readiness is unavailable"))
    }
    pub(super) fn query_matches(&self, command: &[u8], environment: &[(Vec<u8>, Vec<u8>)]) -> bool {
        self.ready.as_ref().is_some_and(|ready| {
            command == ready.invocation.compiler.path
                && environment.iter().all(|(key, value)| match key.as_slice() {
                    b"CARGO" => value == &ready.invocation.program.path,
                    b"RUSTC" => value == &ready.invocation.compiler.path,
                    b"CARGO_HOME" => value == &ready.invocation.cargo_home,
                    b"CARGO_CACHE_RUSTC_INFO" => value == b"0",
                    b"LANG" | b"LC_ALL" => value == b"C",
                    _ => false,
                })
        })
    }
    pub(super) fn authorizes_query(&self, query: &host::CompilerQuery) -> bool {
        self.cooperative_dispatch()
            && query.validate().is_ok()
            && self.ready.as_ref().is_some_and(|ready| {
                query.root == ready.invocation.root
                    && query.program_sha256 == ready.invocation.compiler.sha256
                    && query.member.command.cwd == self.cwd
                    && self.query_matches(
                        &query.member.command.program,
                        &query.member.command.environment,
                    )
                    && query.member.command.environment.len() == self.environment.len()
                    && self
                        .environment
                        .iter()
                        .all(|entry| query.member.command.environment.contains(entry))
            })
    }
}

impl Owner {
    pub(in super::super) fn install_workload_control(
        &mut self,
        peer: Peer,
        session: &str,
        attempt: [u8; 16],
        link: crate::workload_transport::Link,
    ) -> io::Result<()> {
        let job = self
            .jobs
            .get_mut(&(session.to_owned(), attempt))
            .ok_or_else(|| refused("original workload root unavailable"))?;
        if !job.launched
            || job.reaped
            || job.settled
            || job.cleanup_at.is_some()
            || peer.pid != job.child.child_mut().id()
            || peer.birth != job.root_birth
            || !job
                .metadata_sdk
                .as_ref()
                .is_some_and(|sdk| sdk.validate_loaded(peer.pid).is_ok())
        {
            return Err(refused("workload control differs from original live root"));
        }
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("no original workload owner"))?;
        if state.ready.is_none() || state.control.is_some() {
            return Err(refused("workload control is unavailable or replayed"));
        }
        state.control = Some(link);
        Ok(())
    }
    pub(in super::super) fn workload_member_original(
        &mut self,
        peer: Peer,
        request: &host::MemberOriginalRequest,
    ) -> io::Result<MemberOriginalCustody> {
        request.encode()?;
        let context = self.context(peer, &request.query)?;
        let job = self
            .jobs
            .get_mut(&(
                request.query.binding.session.clone(),
                request.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original workload unavailable"))?;
        let sdk = job
            .metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("original workload SDK unavailable"))?;
        let Some((slot, target)) = job
            .members
            .get_mut(&request.member_attempt)
            .and_then(|member| member.workload_primary(peer))
        else {
            return Err(refused(
                "caller is not the live original registered compiler",
            ));
        };
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("registered compiler has no original workload"))?;
        let image = aterm_uds::process::LoadedImage::read(peer.pid as i32)
            .ok_or_else(|| refused("registered compiler image unavailable"))?;
        if state.ready.is_none()
            || state.member_originals.contains(&request.member_attempt)
            || !state.confirmed_primary(request.member_attempt, slot)
            || !state
                .census
                .get(&request.member_attempt)
                .is_some_and(|entry| matches!(entry.kind, CensusKind::Compiler))
            || !matches!(context.context.subject,trust_resource_controller::Subject::RegisteredMember{attempt,..} if attempt==request.member_attempt)
            || !crate::metadata_prelude::no_fork(peer.pid)?
            || image.matches_file(&sdk.execution().join("bin/trustc")) != Some(true)
            || job
                .loans
                .get(&request.member_attempt)
                .is_some_and(|loan| !loan.runnable())
        {
            return Err(refused(
                "registered workload compiler custody or phase differs",
            ));
        }
        sdk.validate()?;
        let lease = state.primary.original_slot(slot)?;
        let mut cpu = state
            .primary
            .observations()?
            .into_iter()
            .find(|cpu| cpu.slot == slot)
            .ok_or_else(|| refused("registered compiler slot is outside original set"))?;
        cpu.inherited_fd = target;
        let reply = host::MemberOriginalReply { context, cpu };
        reply.encode()?;
        let original = aterm_uds::kept_out(|| lease.as_fd().try_clone_to_owned())?;
        state.member_originals.insert(request.member_attempt);
        Ok(MemberOriginalCustody {
            reply,
            original,
            _owner: Arc::clone(&state.primary),
            _sdk: Arc::clone(sdk),
        })
    }
    pub(in super::super) fn workload_ready(
        &mut self,
        peer: Peer,
        request: &host::ReadyRequest,
    ) -> io::Result<ReadyCustody> {
        request.validate()?;
        let context = self.context(peer, &request.query)?;
        let job = self
            .jobs
            .get_mut(&(
                request.query.binding.session.clone(),
                request.query.binding.root_attempt,
            ))
            .ok_or_else(|| refused("original workload root is unavailable"))?;
        let state = job
            .workload
            .as_mut()
            .ok_or_else(|| refused("root has no workload owner"))?;
        let sdk = job
            .metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("original workload SDK missing"))?;
        let invocation = &request.invocation;
        if state.ready.is_some()
            || !job.members.is_empty()
            || !job.loans.is_empty()
            || !matches!(job.custody, Custody::Cooperative(_))
            || peer.pid != job.child.child_mut().id()
            || peer.birth != job.root_birth
            || invocation.root != sdk.execution().as_os_str().as_bytes()
            || invocation.program.path != job.program.as_os_str().as_bytes()
            || invocation.cwd != state.cwd
            || !state
                .environment
                .contains(&(b"CARGO_HOME".to_vec(), invocation.cargo_home.clone()))
            || job
                .timeout
                .is_none_or(|timeout| u128::from(invocation.timeout_ns) > timeout.as_nanos())
            || invocation.program.sha256 != sdk.program_sha256()?
        {
            return Err(refused(
                "workload readiness differs from its actual original owner",
            ));
        }
        sdk.validate_loaded(peer.pid)?;
        let root_deadline = job.started
            + job
                .timeout
                .ok_or_else(|| refused("original workload deadline unavailable"))?;
        let (compiler, deadline) = compiler_input(sdk, root_deadline, Instant::now())?;
        let mut compiler_sha = [0; 32];
        if !aterm_codec::hex::decode_into(
            compiler.observation().executable.sha256.as_bytes(),
            &mut compiler_sha,
        ) || compiler_sha != invocation.compiler.sha256
        {
            return Err(refused(
                "workload compiler differs from the original pre-GO SDK inputs",
            ));
        }
        let output_path =
            std::path::Path::new(std::ffi::OsStr::from_bytes(&invocation.output_root));
        let output = trust_resource_provenance::program::produced::HeldOutputRoot::open(
            output_path,
            deadline,
        )?;
        let sha256 = aterm_digest::Sha256::digest(&invocation.encode()?);
        let mut set = state.primary.observations()?;
        let primary = set.remove(0);
        state.ready = Some(Ready {
            invocation: invocation.clone(),
            sha256,
            root_attempt: request.query.binding.root_attempt,
            _output: Arc::new(output),
        });
        Ok(ReadyCustody {
            reply: host::ReadyReply {
                context,
                invocation_sha256: state.ready_digest()?,
                primary,
                supplementary: set,
            },
            owner: Arc::clone(&state.primary),
            deadline: root_deadline,
        })
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    #[test]
    fn complete_maximum_repetition_budget_remains_finite_without_a_day_cap() {
        let mut request = aterm_types::resource_admission::Submission {
            binding: Binding {
                session: "s-acde".into(),
                attempt: [1; 16],
            },
            peer_pid: std::process::id(),
            peer_birth: 1,
            argv: vec![
                b"/sdk/bin/targo".to_vec(),
                wire::WORKLOAD_STARTUP_ARGUMENT.as_bytes().to_vec(),
            ],
            cwd: b"/work".to_vec(),
            env: Vec::new(),
            cpu_units: 1,
            memory_bytes: 1,
            label: "finite workload profile".into(),
            timeout_ms: Some(261_900_000),
        };
        // 67 real phase budgets at3600s,67 bounded300s handoffs,300s
        // startup and300s settlement. This value is containment DATA only.
        assert!(closed_request(&request));
        for timeout in [
            None,
            Some(0),
            Some(host::MAX_ROOT_TIMEOUT_MS + 1),
            Some(u64::MAX),
        ] {
            request.timeout_ms = timeout;
            assert!(!closed_request(&request));
        }
        request.timeout_ms = Some(1);
        assert!(closed_request(&request));
    }
}
