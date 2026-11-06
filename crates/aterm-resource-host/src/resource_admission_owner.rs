// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical CPU and memory authority. Opening never migrates or repairs a
//! machine. The durable account transition and shared debt domain must exist.
use aterm_capacity::build_cpu::{ExternalDebt, LegacyRetirement, Pool, PreparedRecovery};
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
#[path = "resource_admission_queue.rs"]
mod queue;
#[path = "resource_admission_recipe.rs"]
mod recipe;
#[path = "resource_admission_recovery.rs"]
mod recovery;
#[path = "resource_admission_start.rs"]
mod start;
#[cfg(test)]
#[path = "resource_admission_owner_tests.rs"]
mod tests;

pub(super) struct Owner {
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
}
struct Pending {
    submission: AuthorizedSubmission,
    events: job::Events,
    recipe: Option<recipe::Recipe>,
    probe: Option<std::sync::mpsc::Receiver<io::Result<recipe::Recipe>>>,
    waiter: Option<Waiter>,
    claim: Option<Claim>,
    recovery: Option<PreparedRecovery>,
}
enum Custody {
    Fixed(RetainedCohort),
    Cooperative(Claim),
}
struct Failed {
    binding: Binding,
    custody: Custody,
    spawned: Option<Box<spawnfd::Spawned>>,
    no_child: Option<Box<spawnfd::NotSpawned>>,
    checked_at: Option<Instant>,
}
struct Job {
    binding: Binding,
    delegation: aterm_types::resource_subgroup::JobContext,
    program: PathBuf,
    native_targo: bool,
    members: BTreeMap<[u8; 16], member_queue::Slot>,
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
    launched: bool,
    settled: bool,
    settlement_at: Option<Instant>,
}
fn refused(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, reason)
}

struct Resources {
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
        let capacity = aterm_capacity::ledger::user_dir()
            .ok_or_else(|| refused("canonical CPU parent unavailable"))?;
        let pool = Pool::open_existing(&capacity)?
            .ok_or_else(|| refused("canonical CPU pool unavailable"))?;
        let retirement = LegacyRetirement::open_canonical()?;
        retirement.ensure_prior_boot()?;
        let debt = ExternalDebt::open(&pool, &retirement)?
            .ok_or_else(|| refused("mandatory shared debt domain unavailable"))?;
        let journal = journal::Journal::open(&namespace, &debt)?;
        Ok(Self {
            ledger,
            namespace,
            pool,
            debt,
            journal,
        })
    }
}
impl Owner {
    pub(super) fn idle(&self) -> bool {
        self.pending.is_empty() && self.jobs.is_empty() && self.failed.is_empty()
    }
    pub(super) fn prerequisites() -> io::Result<()> {
        Resources::open().map(|_| ())
    }
    pub(super) fn open() -> io::Result<Self> {
        let Resources {
            ledger,
            namespace,
            pool,
            debt,
            journal,
        } = Resources::open()?;
        let fence = Fence::acquire(&namespace, Instant::now() + Duration::from_millis(100))?;
        let mut owner = Self {
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
        };
        owner.recover()?;
        Ok(owner)
    }
    pub(super) fn capabilities(&self) -> aterm_json::Value {
        let ready = !self.faulted
            && self.fence.check_current().is_ok()
            && self.ledger.cohort_aware().is_ok_and(|ready| ready);
        aterm_json::json!({"version":3,"ready":ready,"recipes":["ay-campaign-v3","targo-native-v3"],"unqualified":"refused","inspection":"resource-inspect-v3","status":"json-lines-v3","timeout":"actual-start"})
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
        if let Err(failure) = self.enqueue(submission) {
            let (submission, _) = *failure;
            super::refuse(submission);
        }
    }
    pub(super) fn tick(&mut self) {
        if self.fence.check_current().is_err() {
            self.faulted = true;
        }
        self.tick_failed();
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
        if !self.faulted {
            self.tick_queue();
        }
    }
    pub(super) fn inspect(&mut self, peer: Peer, session: &str) -> io::Result<Inspection> {
        self.fence.check_current()?;
        if self.faulted {
            return Err(refused("host custody is faulted"));
        }
        for job in self.jobs.values_mut() {
            let root = job.child.child_mut().id();
            if job.binding.session == session
                && !job.reaped
                && !job.settled
                && job.cleanup_at.is_none()
                && aterm_uds::process::stable_descendant(
                    peer.pid,
                    aterm_uds::process::Birth(peer.birth),
                    root,
                    aterm_uds::process::Birth(job.root_birth),
                    Instant::now() + Duration::from_secs(2),
                )
            {
                return job::inspection(&self.fence, &self.debt, job);
            }
            if job.binding.session == session
                && !job.reaped
                && !job.settled
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
