// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded headless job ownership. One caller drives ticks and one native wake set.
//! Submission performs durable/native launch I/O; ticks never wait for a job's exit.

use super::{
    CancelHandle, Cancellation, CohortSettlement, Held, JobControl, JobEvents, JobOutcome,
    SupervisedChild, SupervisedEvent, TreeMeter, launch_held, request_owned_cleanup,
};
use aterm_capacity::cohort::launch::CohortSignal;
use aterm_uds::{
    childwake::ChildWake,
    spawnfd::{self, ChildEvent, ChildNotice, ChildStop, Rusage},
};
use std::{
    collections::BTreeMap,
    io,
    os::fd::{AsFd as _, BorrowedFd},
    sync::Arc,
    time::{Duration, Instant},
};

/// An exact attempt in this table. A number or a different table cannot mint it.
/// Clones remain valid while the same actual table owns the job; removal never
/// reuses an attempt number. This is not serialized restart or process authority.
#[derive(Clone, Debug)]
pub struct JobId {
    table: Arc<()>,
    sequence: u64,
}

impl PartialEq for JobId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.table, &other.table) && self.sequence == other.sequence
    }
}
impl Eq for JobId {}

impl JobId {
    /// Display sequence, unique only within its owning table. Not a control token.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Submitted intent handles; the host retains all actual child authority.
#[derive(Clone, Debug)]
pub struct Ticket {
    pub id: JobId,
    pub cancel: CancelHandle,
}

/// A root's accounted completion and separate durable group settlement. An error
/// preserves the original launch/control/observer failure. Retained budgets may
/// outlive this result; an empty table does not certify that all work drained.
#[derive(Debug)]
pub struct Completion {
    pub id: JobId,
    pub result: io::Result<JobOutcome>,
}

/// Readiness hints only. No bit establishes child state, signal acceptance or drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wake {
    pub control_ready: bool,
    pub jobs_hint: bool,
    pub deadline: bool,
}

pub use aterm_capacity::host_protocol::Phase;

/// Owner-side diagnostics from the same table that controls actual jobs.
#[derive(Clone, Debug)]
pub struct JobInfo {
    pub id: JobId,
    pub phase: Phase,
    pub reserved_bytes: u64,
    /// Cleanup authority/end request or an actual terminal root permits reap;
    /// this is not a signal-acceptance or group-drain receipt.
    pub cleanup_reap_allowed: bool,
    pub failure: Option<String>,
}

/// A process-affine bounded table, independent of terminal/global signal fronts.
/// One tick services each job once. Callbacks must return to that loop. The host
/// owns its SIGCHLD/default-retained-child and shutdown policies and bounds launch
/// I/O outside any UI thread. Existing TreeMeter sampler threads remain.
///
/// Drop is emergency shutdown: it requests authorized termination for all owned
/// roots before waiting to reap them. It may block and must not run on a UI thread.
/// Refused custody is never bypassed; its complete budget stays durable.
pub struct Jobs {
    owner: u32,
    seal: Arc<()>,
    next: u64,
    limit: usize,
    wake: Arc<ChildWake>,
    jobs: BTreeMap<u64, NativeJob>,
}

impl Jobs {
    /// Create one kernel child-state watch before any submitted native birth.
    /// # Errors
    /// Limit outside 1..=256, incompatible child policy or native watch failure.
    pub fn new(limit: usize) -> io::Result<Self> {
        if !(1..=256).contains(&limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job table limit must be 1..=256",
            ));
        }
        Ok(Self {
            owner: std::process::id(),
            seal: Arc::new(()),
            next: 1,
            limit,
            wake: Arc::new(ChildWake::new()?),
            jobs: BTreeMap::new(),
        })
    }

    fn check_owner(&self) -> io::Result<()> {
        if self.owner == std::process::id() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "job table belongs to another process",
            ))
        }
    }

    /// Number of actual roots still owned, not a claim that all cohorts drained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }
    /// Whether no actual roots remain in this table. Retained budgets can remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Read bounded owner-side state without granting new process authority.
    /// Waiting means no new native observation, not that a CPU is idle. A failed
    /// job remains owned until actual reap; unknown custody still holds its budget.
    /// # Errors
    /// The table belongs to another process.
    pub fn snapshot(&self) -> io::Result<Vec<JobInfo>> {
        self.check_owner()?;
        Ok(self
            .jobs
            .iter()
            .map(|(&sequence, job)| JobInfo {
                id: JobId {
                    table: Arc::clone(&self.seal),
                    sequence,
                },
                phase: job.phase(),
                reserved_bytes: job
                    .held
                    .as_ref()
                    .expect("owned admission")
                    .claim()
                    .record()
                    .need
                    .mem,
                cleanup_reap_allowed: job.cleanup_issued,
                failure: job.failure.as_ref().map(ToString::to_string),
            })
            .collect())
    }

    // Public projection never even formats arbitrary private diagnostic payloads.
    pub(super) fn protocol_rows(&self) -> io::Result<Vec<aterm_capacity::host_protocol::Job>> {
        self.check_owner()?;
        Ok(self
            .jobs
            .iter()
            .map(|(&attempt, job)| aterm_capacity::host_protocol::Job {
                attempt,
                phase: job.phase(),
                reserved_bytes: job
                    .held
                    .as_ref()
                    .expect("owned admission")
                    .claim()
                    .record()
                    .need
                    .mem,
                failure: job
                    .failure
                    .as_ref()
                    .map(aterm_capacity::host_protocol::Failure::of),
            })
            .collect())
    }

    /// Durably launch a fully admitted job, stopped until a later tick acknowledges
    /// its initial stop. A failed BIND keeps the actual root in this table and
    /// returns its ticket; its original error is delivered after cleanup/reap.
    /// Keep at least one cancellation sender alive until completion.
    /// # Errors
    /// Foreign owner, full/exhausted table, non-cohort admission, invalid grace,
    /// native refusal or authoritative no-child failure. No existing job changes.
    pub fn submit(
        &mut self,
        mut held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
    ) -> io::Result<Ticket> {
        self.check_owner()?;
        if self.jobs.len() == self.limit {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "owned job table is full",
            ));
        }
        if !held.cohort {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "job table requires native cohort admission",
            ));
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("job attempt sequence exhausted"))?;
        let (cancel, cancellation) = Cancellation::channel_on(grace, Arc::clone(&self.wake))?;
        command.env(
            aterm_capacity::COVER_ENV,
            held.claim().record().cover_marker(),
        );
        struct Restore<'a> {
            command: &'a mut spawnfd::Command,
            configured: bool,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.command.start_suspended(self.configured);
            }
        }
        let configured = command.starts_suspended();
        command.start_suspended(true);
        let restore = Restore {
            command,
            configured,
        };
        let (mut owned, failure) = launch_held(Some(&mut held), restore.command)?;
        let meter = TreeMeter::start(owned.child_mut().id());
        let id = JobId {
            table: Arc::clone(&self.seal),
            sequence: self.next,
        };
        self.jobs.insert(
            self.next,
            NativeJob {
                held: Some(held),
                owned,
                meter: Some(meter),
                cancellation,
                starting: true,
                stopped: false,
                cleanup_issued: false,
                usage: None,
                failure,
            },
        );
        self.next = next;
        Ok(Ticket { id, cancel })
    }

    fn find_mut(&mut self, id: &JobId) -> io::Result<&mut NativeJob> {
        self.check_owner()?;
        if !Arc::ptr_eq(&self.seal, &id.table) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "job attempt belongs to another table",
            ));
        }
        self.jobs
            .get_mut(&id.sequence)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "job attempt already completed"))
    }

    /// Resume only an actually stopped owned job, without terminal foreground.
    /// Returns the actual accepted observation directly to the requesting host.
    /// # Errors
    /// Foreign/stale ID, job not stopped, active cancellation/failure or native refusal.
    pub fn resume_background(&mut self, id: &JobId) -> io::Result<SupervisedEvent> {
        let job = self.find_mut(id)?;
        if !job.stopped || job.failure.is_some() || job.cancellation.requested() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job is not resumable",
            ));
        }
        job.owned.request_group(CohortSignal::Continue)?;
        job.stopped = false;
        Ok(SupervisedEvent::ResumeAccepted { foreground: false })
    }

    /// Service each owned job once. One queued intent and at most one ordinary
    /// normal child observation per job prevent another mailbox starving peers.
    /// A failed job also gets one bounded cleanup observation.
    /// Completed roots are removed exactly once. Errors in one job trigger its
    /// authorized cleanup while all peers still receive their turn.
    /// # Errors
    /// Table belongs to another process or its kernel wake source failed.
    pub fn tick(
        &mut self,
        events: &mut dyn FnMut(&JobId, SupervisedEvent) -> io::Result<JobControl>,
        say: &mut dyn FnMut(&str),
    ) -> io::Result<Vec<Completion>> {
        self.check_owner()?;
        self.wake.drain()?;
        let mut complete = Vec::new();
        for (&sequence, job) in &mut self.jobs {
            let id = JobId {
                table: Arc::clone(&self.seal),
                sequence,
            };
            let mut policy: JobEvents<'_> = &mut |event| events(&id, event);
            if job.failure.is_none()
                && let Err(error) = job.step(&mut policy)
            {
                job.failure = Some(error);
            }
            if job.failure.is_some() && job.usage.is_none() {
                if !job.cleanup_issued {
                    job.cleanup_issued = request_owned_cleanup(&mut job.owned, say);
                }
                // Cleanup validates the original scope/seal and tries surviving
                // members before permitting the owning root reap.
                if job.cleanup_issued {
                    let _ = job.observe(&mut policy, false);
                }
            }
            if job.usage.is_some() {
                complete.push(id);
            }
        }
        Ok(complete
            .into_iter()
            .map(|id| {
                let job = self.jobs.remove(&id.sequence).expect("completed owned job");
                let mut policy: JobEvents<'_> = &mut |event| events(&id, event);
                let result = job.finish(&mut policy, say);
                Completion { id, result }
            })
            .collect())
    }

    /// Wait once for host control, any job's inbox, child-state hints or the
    /// nearest grace/caller deadline. Reads no control bytes. Tick before wait;
    /// hints may concern another child and must be checked against actual owners.
    /// # Errors
    /// Foreign process owner or native readiness failure.
    pub fn wait(&self, control: BorrowedFd<'_>, until: Instant) -> io::Result<Wake> {
        self.check_owner()?;
        let mut fds = vec![control, self.wake.as_fd()];
        let mut deadline = until;
        for job in self.jobs.values() {
            fds.push(job.cancellation.inbox());
            if let Some(at) = job.cancellation.deadline() {
                deadline = deadline.min(at);
            }
        }
        let ready = aterm_uds::ready::any_until(&fds, deadline)?;
        Ok(Wake {
            control_ready: ready.contains(&0),
            jobs_hint: ready.iter().any(|&i| i != 0),
            deadline: ready.is_empty(),
        })
    }
}

struct NativeJob {
    held: Option<Held>,
    owned: SupervisedChild,
    meter: Option<TreeMeter>,
    cancellation: Cancellation,
    starting: bool,
    stopped: bool,
    cleanup_issued: bool,
    usage: Option<Rusage>,
    failure: Option<io::Error>,
}

impl NativeJob {
    fn phase(&self) -> Phase {
        if self.failure.is_some() {
            Phase::Faulted
        } else if self.starting {
            Phase::Starting
        } else if self.cancellation.exit_observed() && self.cancellation.holds_root() {
            Phase::ExitPinned
        } else if self.cancellation.requested() {
            Phase::Cancelling
        } else if self.stopped {
            Phase::Stopped
        } else {
            Phase::Waiting
        }
    }
    fn step(&mut self, events: &mut JobEvents<'_>) -> io::Result<()> {
        if self.starting {
            let Some(observed) = self.owned.child_mut().try_job_event()? else {
                return Ok(());
            };
            match observed.acknowledge()? {
                Some(ChildEvent::Stopped(ChildStop::SuspendedStart)) => self.starting = false,
                Some(ChildEvent::Exited(usage)) => {
                    self.usage = Some(usage);
                    return super::notify_job_exit(usage, events);
                }
                event => {
                    return Err(io::Error::other(format!(
                        "job table expected suspended native start: {event:?}"
                    )));
                }
            }
            self.cancellation.service(&mut self.owned, events)?;
            if !self.cancellation.requested() {
                self.owned.child_mut().resume()?;
            }
            return Ok(());
        }
        self.cancellation.service(&mut self.owned, events)?;
        self.observe(events, true)
    }

    fn observe(&mut self, events: &mut JobEvents<'_>, decisions: bool) -> io::Result<()> {
        if self.cancellation.exit_observed() && self.cancellation.holds_root() && decisions {
            return Ok(());
        }
        let Some(observed) = self.owned.child_mut().try_job_event()? else {
            return Ok(());
        };
        if observed.notice() == ChildNotice::Exited && self.cancellation.holds_root() && decisions {
            return self.cancellation.observe_exit(events);
        }
        match observed.acknowledge()? {
            Some(ChildEvent::Exited(usage)) => {
                self.usage = Some(usage);
                super::notify_job_exit(usage, events)
            }
            Some(ChildEvent::Stopped(ChildStop::Signal(signal))) if decisions => {
                self.stopped = true;
                match events(SupervisedEvent::Stopped(signal))? {
                    JobControl::Wait => Ok(()),
                    JobControl::ResumeBackground => {
                        self.owned.request_group(CohortSignal::Continue)?;
                        self.stopped = false;
                        if events(SupervisedEvent::ResumeAccepted { foreground: false })?
                            == JobControl::Wait
                        {
                            Ok(())
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "resume decisions require an actual stop",
                            ))
                        }
                    }
                    JobControl::TerminateGroup => {
                        self.cancellation.terminate(&mut self.owned, events)
                    }
                    JobControl::KillGroup => self.cancellation.kill(&mut self.owned, events),
                    JobControl::ResumeForeground => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "headless job has no foreground terminal lease",
                    )),
                }
            }
            Some(ChildEvent::Stopped(_)) if decisions => {
                Err(io::Error::other("unexpected suspended-start notification"))
            }
            _ => Ok(()),
        }
    }

    fn finish(
        mut self,
        events: &mut JobEvents<'_>,
        say: &mut dyn FnMut(&str),
    ) -> io::Result<JobOutcome> {
        let usage = self.usage.expect("only accounted roots finish");
        let mut held = self.held.take().expect("owned admission");
        let settlement = match held.claim.finish_cohort() {
            Ok(()) => CohortSettlement::Drained,
            Err(error) => {
                say(&format!(
                    "build-admit: host job reservation retained ({error})"
                ));
                CohortSettlement::Retained
            }
        };
        let event = if settlement == CohortSettlement::Drained {
            SupervisedEvent::CohortDrained
        } else {
            SupervisedEvent::CohortRetained
        };
        let notified = events(event).and_then(|decision| {
            if decision == JobControl::Wait {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "settlement carries no live child authority",
                ))
            }
        });
        if self.failure.is_none() {
            self.failure = notified.err();
        }
        let outcome = JobOutcome {
            root: usage.status,
            cancelled: self.cancellation.requested(),
            settlement: Some(settlement),
        };
        let peak = self.meter.take().expect("owned meter").stop(usage.maxrss);
        held.let_go()
            .keep(self.failure.is_none() && outcome.succeeded(), peak, say);
        self.failure.map_or(Ok(outcome), Err)
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        if self.owner != std::process::id() {
            return;
        }
        for job in self.jobs.values_mut() {
            if !job.owned.child_mut().is_reaped() {
                job.cleanup_issued |= request_owned_cleanup(&mut job.owned, &mut |_| {});
            }
        }
        for job in self.jobs.values_mut() {
            if job.cleanup_issued {
                let _ = job.owned.child_mut().wait();
            }
        }
    }
}
