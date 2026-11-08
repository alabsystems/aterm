// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical CPU and memory authority. Opening never migrates or repairs a
//! machine. The durable account transition and shared debt domain must exist.
#[cfg(test)]
use aterm_capacity::build_cpu::LegacyRetirement;
use aterm_capacity::build_cpu::{ExternalDebt, NativeDomain, Pool, PreparedRecovery};
use aterm_capacity::{
    Claim, Ledger, Live, Waiter, cohort_namespace::Namespace, leadership::Fence,
    retained_cohort::RetainedCohort,
};
use aterm_ctl::resource_admission_transport::{AuthorizedSubmission, Peer};
use aterm_types::{
    resource_admission::{Binding, CompletionCause},
    resource_inspection::Inspection,
};
use aterm_uds::spawnfd;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

#[path = "resource_admission_compiler.rs"]
mod compiler;
#[path = "resource_admission_context.rs"]
mod context;
#[path = "resource_admission_failure.rs"]
mod failure;
#[path = "resource_admission_job.rs"]
mod job;
#[path = "resource_admission_journal.rs"]
mod journal;
#[path = "resource_admission_member.rs"]
mod member;
#[path = "resource_admission_member_custody.rs"]
mod member_custody;
#[path = "resource_admission_member_queue.rs"]
mod member_queue;
#[path = "resource_admission_member_recipe.rs"]
mod member_recipe;
#[path = "resource_admission_members.rs"]
pub(super) mod members;
#[path = "resource_admission_metadata.rs"]
pub(super) mod metadata;
#[path = "resource_admission_inputs.rs"]
mod native_inputs;
#[path = "resource_admission_parent_loan.rs"]
mod parent_loan;

#[path = "resource_admission_preparation.rs"]
mod preparation;
#[path = "resource_admission_proxy.rs"]
mod proxy;
#[path = "resource_admission_queue.rs"]
mod queue;
#[path = "resource_admission_recipe.rs"]
mod recipe;
#[path = "resource_admission_recovery.rs"]
mod recovery;
#[path = "resource_admission_resolver_root.rs"]
mod resolver_root;
#[path = "resource_admission_sdk.rs"]
mod sdk;
#[path = "resource_admission_settlement.rs"]
mod settlement;
#[path = "resource_admission_start.rs"]
mod start;
#[cfg(test)]
#[path = "resource_admission_owner_tests.rs"]
mod tests;
#[path = "resource_admission_workload.rs"]
pub(super) mod workload;

pub(super) struct Owner {
    // Retain the exact validated canonical domain; tests use a fresh private scope.
    _domain: Option<NativeDomain>,
    fence: Fence,
    ledger: Ledger,
    pool: Pool,
    debt: ExternalDebt,
    journal: journal::Journal,
    pending: VecDeque<Pending>,
    jobs: BTreeMap<(String, [u8; 16]), Job>,
    faulted: bool,
    recovered_at: Instant,
    failed: Vec<Failed>,
    #[cfg(test)]
    recipe_layout: Option<atpkg::store::Layout>,
    #[cfg(all(test, target_os = "macos"))]
    metadata_pause: Option<preparation::Pause>,
}
struct Pending {
    queued_at: Instant,
    submission: AuthorizedSubmission,
    events: job::Events,
    recipe: Option<recipe::Recipe>,
    probe: Option<recipe::Probe>,
    primary: Option<std::sync::Arc<workload::Primary>>,
    sdk: Option<trust_resource_controller::SdkCustody>,
    waiter: Option<MemoryWait>,
    claim: Option<Claim>,
    recovery: Option<PreparedRecovery>,
    admitted: Option<Custody>,
    // Captured only from this pending item's actual Claim before custody moves.
    scope_reference: Option<aterm_capacity::CohortClosureRef>,
    cancelled: bool,
    resolver: Option<resolver_root::Preparation>,
}
impl Pending {
    /// The new workload's original whole-attempt clock includes admission.
    /// Repeated inspections and final GO cannot renew that selected bound.
    fn workload_deadline(&self) -> io::Result<Option<Instant>> {
        if !workload::purpose(&self.submission.request) {
            return Ok(None);
        }
        let millis = self
            .submission
            .request
            .timeout_ms
            .filter(|ms| {
                *ms > 0 && *ms <= trust_resource_controller::workload::host::MAX_ROOT_TIMEOUT_MS
            })
            .ok_or_else(|| refused("original workload deadline unavailable"))?;
        self.queued_at
            .checked_add(Duration::from_millis(millis))
            .map(Some)
            .ok_or_else(|| refused("original workload deadline is unrepresentable"))
    }
}
/// A request's memory wait in this user's ledger, and the `kind=admission` row its admission owes
/// ([`aterm_capacity::telemetry::Owed`], Continuum TM-1): written at the look that decides it,
/// or, given up undecided (the request cancelled, or refused for another reason before its
/// claim), as it drops, after its waiter has left the queue. The owner's thread lives as long as
/// the host's process, so a wait still open when the process ends writes none.
struct MemoryWait {
    waiter: Waiter,
    owed: aterm_capacity::telemetry::Owed,
}
enum Custody {
    Fixed(RetainedCohort),
    Cooperative(Claim),
}
struct Failed {
    _compiler: Option<compiler::Selection>,
    binding: Binding,
    custody: Option<Custody>,
    spawned: Option<Box<spawnfd::Spawned>>,
    no_child: Option<Box<spawnfd::NotSpawned>>,
    checked_at: Option<Instant>,
    _sdk: Option<std::sync::Arc<sdk::Keeper>>,
    _primary: Option<std::sync::Arc<workload::Primary>>,
    refusal: Option<(job::Events, String)>,
    settled: bool,
}
struct Job {
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    control_eof_before_reap_test: bool,
    binding: Binding,
    delegation: aterm_types::resource_subgroup::JobContext,
    program: PathBuf,
    native_targo: bool,
    metadata_root: bool,
    metadata_ready: bool,
    resolver_ready: bool,
    resolver_queries: usize,
    sdk: Option<trust_resource_controller::SdkCustody>,
    members: BTreeMap<[u8; 16], member_queue::Slot>,
    contained_retirements: BTreeMap<[u8; 16], std::sync::Arc<member_custody::ProducerRetirement>>,
    loans: BTreeMap<[u8; 16], parent_loan::RunState>,
    workload: Option<workload::State>,
    // Preparing members join on drop. Keep the original transport and SDK
    // owners until every member has joined or retained uncertain custody.
    metadata_gnu: Option<metadata::RootGnu>,
    metadata_sdk: Option<std::sync::Arc<sdk::Keeper>>,
    resolver: Option<resolver_root::Preparation>,
    compiler: Option<compiler::Selection>,
    events: job::Events,
    custody: Custody,
    child: aterm_capacity::cohort::launch::CohortChild,
    root_birth: u64,
    cpu_ceiling: u32,
    memory_limit: u64,
    memory_at: Instant,
    memory_fault: bool,
    started: Instant,
    ended: Option<Instant>,
    exit_moment: Option<aterm_uds::exitwatch::ExitMoment<Instant>>,
    timeout: Option<Duration>,
    cause: CompletionCause,
    cleanup_at: Option<Instant>,
    reaped: bool,
    terminal_sent: bool,
    completion: Option<(
        aterm_types::resource_admission::Event,
        aterm_types::resource_admission::AccountingObservation,
    )>,
    launched: bool,
    closed: Option<settlement::ClosedRoot>,
    settlement_at: Option<Instant>,
}
impl Job {
    fn is_settled(&self) -> bool {
        self.closed
            .as_ref()
            .is_some_and(|closed| closed.matches(self))
    }
}
fn refused(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, reason)
}

struct Resources {
    domain: NativeDomain,
    ledger: Ledger,
    namespace: Namespace,
    pool: Pool,
    debt: ExternalDebt,
    journal: journal::Journal,
}
impl Resources {
    fn open() -> io::Result<Self> {
        if !cfg!(target_os = "macos") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "retained execution requires a qualified controlled native launch",
            ));
        }
        let ledger =
            Ledger::user().ok_or_else(|| refused("canonical account ledger unavailable"))?;
        if !ledger.cohort_aware()? {
            return Err(refused("canonical memory custody has not been adopted"));
        }
        let namespace = Namespace::resolve(ledger.dir())?
            .ok_or_else(|| refused("canonical retained namespace unavailable"))?;
        let domain = NativeDomain::open_canonical()?;
        let pool = domain.pool().clone();
        let debt = ExternalDebt::open(&pool, domain.legacy())?
            .ok_or_else(|| refused("mandatory shared debt domain unavailable"))?;
        let journal = journal::Journal::open(&namespace, &debt)?;
        Ok(Self {
            domain,
            ledger,
            namespace,
            pool,
            debt,
            journal,
        })
    }
}
impl Owner {
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn control_eof_before_reap_test(&mut self, binding: &Binding) -> bool {
        let Some(job) = self
            .jobs
            .get_mut(&(binding.session.clone(), binding.attempt))
        else {
            return false;
        };
        if !job.launched || job.reaped || job.is_settled() {
            return false;
        }
        // Order the real control EOF before the owning reap. This touches no
        // target status, signal or scope receipt; original custody stays held.
        job.control_eof_before_reap_test = true;
        true
    }
    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn cancel_test(&mut self) {
        for pending in &mut self.pending {
            pending.events.abandon();
        }
        for job in self.jobs.values_mut() {
            job.events.abandon();
        }
    }
    pub(super) fn idle(&self) -> bool {
        self.pending.is_empty() && self.jobs.is_empty() && self.failed.is_empty()
    }
    pub(super) fn prerequisites() -> io::Result<()> {
        Resources::open().map(|_| ())
    }
    pub(super) fn open() -> io::Result<Self> {
        let Resources {
            domain,
            ledger,
            namespace,
            pool,
            debt,
            journal,
        } = Resources::open()?;
        let fence = Fence::acquire(&namespace, Instant::now() + Duration::from_millis(100))?;
        let mut owner = Self {
            _domain: Some(domain),
            fence,
            ledger,
            pool,
            debt,
            journal,
            pending: VecDeque::new(),
            jobs: BTreeMap::new(),
            faulted: false,
            recovered_at: Instant::now(),
            failed: Vec::new(),
            #[cfg(test)]
            recipe_layout: None,
            #[cfg(all(test, target_os = "macos"))]
            metadata_pause: None,
        };
        owner.recover()?;
        Ok(owner)
    }
    pub(super) fn capabilities(&self) -> aterm_json::Value {
        let ready = !self.faulted
            && self.fence.check_current().is_ok()
            && self.ledger.cohort_aware().is_ok_and(|ready| ready);
        aterm_json::json!({"version":3,"ready":ready,"recipes":["ay-campaign-v3","targo-native-v3"],"unqualified":"refused","inspection":"resource-inspect-v3","status":"json-lines-v3","timeout":"actual-start","accounting":crate::resource_accounting::supported().then_some(trust_resource_controller::accounting::CAPABILITY),"root_store_expectation":crate::runtime::store_expectation_supported().then_some(aterm_ctl::resource_admission_transport::store_expectation::CAPABILITY)})
    }
    pub(super) fn accepts(&self, binding: &Binding) -> bool {
        !self.faulted
            && self.pending.len()
                + self.jobs.len()
                + self
                    .jobs
                    .values()
                    .map(|job| job.members.len())
                    .sum::<usize>()
                < aterm_capacity::host_protocol::MAX_JOBS
            && self.fence.check_current().is_ok()
            && self.journal.fresh(binding).unwrap_or(false)
    }
    pub(super) fn submit(&mut self, submission: AuthorizedSubmission) {
        self.submit_mode(submission, false);
    }
    pub(super) fn submit_mode(&mut self, submission: AuthorizedSubmission, accounted: bool) {
        if let Err(failure) = self.enqueue(submission, accounted) {
            let (submission, _) = *failure;
            super::refuse_mode(submission, accounted);
        }
    }
    pub(super) fn tick(&mut self) {
        if self.fence.check_current().is_err() {
            self.faulted = true;
        }
        self.tick_failed();
        self.tick_workload_registrations();
        self.tick_members();
        if self.faulted {
            for job in self
                .jobs
                .values_mut()
                .filter(|job| !job.reaped && job.cleanup_at.is_none())
            {
                job.cause = CompletionCause::ResourceLimit;
                job.cleanup_at = Some(Instant::now());
                if job
                    .child
                    .request_group(aterm_capacity::cohort::launch::CohortSignal::Kill)
                    .is_err()
                {
                    let _ = job.child.child_mut().kill();
                }
            }
        }
        self.tick_jobs();
        if !self.faulted && self.recovered_at.elapsed() >= Duration::from_secs(1) {
            self.recovered_at = Instant::now();
            if self.recover().is_err() {
                self.faulted = true;
            }
        }
        if self.faulted {
            for pending in &mut self.pending {
                pending.cancelled = true;
            }
        }
        // Paid preparation must still join on a failed owner. This branch can
        // only cancel; it never admits another job or releases uncertain debt.
        self.tick_queue();
    }
    pub(super) fn inspect(&mut self, peer: Peer, session: &str) -> io::Result<Inspection> {
        self.inspect_at(peer, session, Instant::now())
    }
    fn inspect_at(
        &mut self,
        peer: Peer,
        session: &str,
        started: Instant,
    ) -> io::Result<Inspection> {
        self.fence.check_current()?;
        if self.faulted {
            return Err(refused("host custody is faulted"));
        }
        for job in self.jobs.values_mut() {
            let root = job.child.child_mut().id();
            let deadline = started + Duration::from_secs(60);
            if job.binding.session == session
                && !job.reaped
                && !job.is_settled()
                && job.cleanup_at.is_none()
                && aterm_uds::process::stable_descendant(
                    peer.pid,
                    aterm_uds::process::Birth(peer.birth),
                    root,
                    aterm_uds::process::Birth(job.root_birth),
                    deadline,
                )
            {
                return job::inspection(&self.fence, &self.debt, job);
            }
            if job.binding.session == session
                && !job.reaped
                && !job.is_settled()
                && job.cleanup_at.is_none()
            {
                let subject = job.members.values_mut().find_map(|member| {
                    if member.covers(peer) {
                        member.subject()
                    } else {
                        None
                    }
                });
                if let Some(subject) = subject {
                    let mut inspection = job::inspection(&self.fence, &self.debt, job)?;
                    inspection.subject = subject;
                    inspection.json()?;
                    return Ok(inspection);
                }
            }
        }
        Err(refused(
            "calling process has no authenticated original host reservation",
        ))
    }
}

impl Owner {
    pub(super) fn wake_after(&self) -> Duration {
        if self.idle() {
            // Admission messages wake the receiver immediately. Idle owners
            // need only the existing one-second orphan-recovery cadence.
            return Duration::from_secs(1);
        }
        self.jobs
            .values()
            .filter(|job| !job.reaped && job.launched && job.cleanup_at.is_none())
            .filter_map(|job| {
                job.timeout
                    .map(|limit| limit.saturating_sub(job.started.elapsed()))
            })
            .fold(Duration::from_millis(50), Duration::min)
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(super) fn metadata_native_role() {
    tests::metadata_native_role();
}

#[cfg(all(test, target_os = "macos"))]
pub(super) fn member_fixture(
    args: &[std::ffi::OsString],
    gnu: Option<[std::os::fd::OwnedFd; 2]>,
) -> std::process::ExitCode {
    tests::member_fixture(args, gnu)
}
