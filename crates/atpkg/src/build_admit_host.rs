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
    fmt, io,
    os::fd::{AsFd as _, BorrowedFd},
    sync::Arc,
    time::{Duration, Instant},
};

const SETTLEMENT_RETRY: Duration = Duration::from_millis(100);

#[path = "build_admit_returning.rs"]
mod returning;
use returning::{Retained, ReturnMode};

/// A refusal before dispatch, returning the original admission and resources.
/// Once installed in the owner, a failed launch keeps its ticket instead.
pub struct RetainedRefused<T> {
    error: io::Error,
    inputs: Box<(Held, T)>,
}

impl<T> RetainedRefused<T> {
    pub(super) fn new(error: io::Error, held: Held, retained: T) -> Self {
        Self {
            error,
            inputs: Box::new((held, retained)),
        }
    }

    /// Why dispatch refused without consuming the original resource custody.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Recover the original admission and resource bundle; no target was dispatched.
    #[must_use]
    pub fn into_parts(self) -> (io::Error, Held, T) {
        let (held, retained) = *self.inputs;
        (self.error, held, retained)
    }
}

impl<T> fmt::Debug for RetainedRefused<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedRefused")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Display for RetainedRefused<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "retained job admission refused: {}", self.error)
    }
}

impl<T> std::error::Error for RetainedRefused<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

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
    // Only this table's actual finish constructs a return capability. The public
    // result and job identity remain observations, not resource-release authority.
    returned: Option<returning::Returned>,
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
    /// Actual resources stay in this table through unresolved cohort settlement.
    pub retained_resources: bool,
    /// Positive owning root reap, distinct from native cohort settlement.
    pub root_reaped: bool,
    /// The original claim could not yet establish native drain or exact cancellation.
    pub settlement_failure: Option<String>,
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

    /// Number of owned roots and retained operations awaiting native settlement.
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }
    /// Whether no root or retained operation remains. Legacy budgets can remain.
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
                retained_resources: job.retained.is_some(),
                root_reaped: job.usage.is_some(),
                settlement_failure: job.settlement_failure.as_ref().map(ToString::to_string),
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
                owned: Some(owned),
                meter: Some(meter),
                cancellation,
                starting: true,
                stopped: false,
                cleanup_issued: false,
                usage: None,
                failure,
                retained: None,
                settled: false,
                settlement_retry: None,
                settlement_failure: None,
            },
        );
        self.next = next;
        Ok(Ticket { id, cancel })
    }

    /// Install actual resources before native birth and keep the original claim until drain.
    /// Every later launch/BIND/observer error remains an owned attempt, including an
    /// uncertain ARM without a child handle. Completion releases resources only after
    /// a positive root reap (when there is a child) and successful native settlement.
    ///
    /// # Errors
    /// A preflight refusal returns the untouched admission and resource bundle.
    /// No command has been dispatched when this method returns `Err`.
    pub fn submit_retained<T: Send + 'static>(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
        retained: T,
    ) -> Result<Ticket, RetainedRefused<T>> {
        self.submit_retained_with(held, command, grace, retained, NativeJob::dispatch)
    }

    fn submit_retained_with<T: Send + 'static>(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
        retained: T,
        dispatch: impl FnOnce(&mut NativeJob, &mut spawnfd::Command),
    ) -> Result<Ticket, RetainedRefused<T>> {
        self.submit_retained_mode_with(
            held,
            command,
            grace,
            retained,
            ReturnMode::DropAfterSettlement,
            dispatch,
        )
    }

    /// Retain an original materialization bundle through native settlement and
    /// return it in this job's own successful completion. A failed completion
    /// keeps its bundle; no observation can reconstruct it or claim seed validity.
    ///
    /// # Errors
    /// A preflight refusal returns the untouched admission and bundle before dispatch.
    pub fn submit_returning<T: Send + 'static>(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
        retained: T,
    ) -> Result<Ticket, RetainedRefused<T>> {
        self.submit_retained_mode_with(
            held,
            command,
            grace,
            retained,
            ReturnMode::ReturnAfterSuccess,
            NativeJob::dispatch,
        )
    }

    fn submit_retained_mode_with<T: Send + 'static>(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: Duration,
        retained: T,
        mode: ReturnMode,
        dispatch: impl FnOnce(&mut NativeJob, &mut spawnfd::Command),
    ) -> Result<Ticket, RetainedRefused<T>> {
        let prepared = (|| {
            self.check_owner()?;
            if self.jobs.len() == self.limit || !held.cohort {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "retained job needs available native cohort admission",
                ));
            }
            let next = self
                .next
                .checked_add(1)
                .ok_or_else(|| io::Error::other("job attempt sequence exhausted"))?;
            let channel = Cancellation::channel_on(grace, Arc::clone(&self.wake))?;
            Ok((next, channel))
        })();
        let (next, (cancel, cancellation)) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Err(RetainedRefused::new(error, held, retained)),
        };
        let id = JobId {
            table: Arc::clone(&self.seal),
            sequence: self.next,
        };
        self.next = next;
        self.jobs.insert(
            id.sequence,
            NativeJob {
                held: Some(held),
                owned: None,
                meter: None,
                cancellation,
                starting: true,
                stopped: false,
                cleanup_issued: false,
                usage: None,
                failure: None,
                retained: Some(Retained::new(mode, retained)),
                settled: false,
                settlement_retry: None,
                settlement_failure: None,
            },
        );
        // The table already owns admission and guards if dispatch or metering unwinds.
        let job = self
            .jobs
            .get_mut(&id.sequence)
            .expect("installed retained job");
        dispatch(job, command);
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
        if !job.stopped
            || job.failure.is_some()
            || job.cancellation.requested()
            || job.usage.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job is not resumable",
            ));
        }
        job.owned
            .as_mut()
            .ok_or_else(|| io::Error::other("job has no owned child"))?
            .request_group(CohortSignal::Continue)?;
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
            if job.failure.is_some() && job.usage.is_none() && job.owned.is_some() {
                if !job.cleanup_issued {
                    job.cleanup_issued =
                        request_owned_cleanup(job.owned.as_mut().expect("owned child"), say);
                }
                // Cleanup validates the original scope/seal and tries surviving
                // members before permitting the owning root reap.
                if job.cleanup_issued {
                    let _ = job.observe(&mut policy, false);
                }
            }
            if if job.retained.is_some() {
                job.settle_retained(Instant::now())
            } else {
                job.usage.is_some()
            } {
                complete.push(id);
            }
        }
        Ok(complete
            .into_iter()
            .map(|id| {
                let job = self.jobs.remove(&id.sequence).expect("completed owned job");
                let mut policy: JobEvents<'_> = &mut |event| events(&id, event);
                let (result, returned) = job.finish(&mut policy, say);
                Completion {
                    id,
                    result,
                    returned,
                }
            })
            .collect())
    }

    /// Wait once for host control, any job's inbox, child-state hints or the
    /// nearest grace/caller deadline. Reads no control bytes. Tick before wait;
    /// hints may concern another child and must be checked against actual owners.
    /// # Errors
    /// Foreign process owner or native readiness failure.
    pub fn wait(&self, control: BorrowedFd<'_>, until: Instant) -> io::Result<Wake> {
        self.wait_with(control, &[], false, until)
    }

    /// Add owned subsystems' opaque read/write interests to the existing control,
    /// cancellation and child-state wait. Buffered work returns a job hint at
    /// once while still checking native control and cancellation readiness.
    /// The caller bounds extras and shortens `until` for paced retries.
    /// Nothing here reads protocol bytes or establishes resource completion.
    /// # Errors
    /// Foreign process owner or native readiness failure.
    pub fn wait_with(
        &self,
        control: BorrowedFd<'_>,
        extra: &[aterm_uds::ready::Watch<'_>],
        immediate: bool,
        until: Instant,
    ) -> io::Result<Wake> {
        use aterm_uds::ready::{Interest, Watch};
        self.check_owner()?;
        let mut fds = vec![
            Watch::new(control, Interest::Read),
            Watch::new(self.wake.as_fd(), Interest::Read),
        ];
        let mut deadline = until;
        for job in self.jobs.values() {
            // A settled leader/uncertain no-handle attempt has no serviced control
            // path. Watching its unread late intents would busy-wake forever.
            if job.owned.is_some() && job.usage.is_none() {
                fds.push(Watch::new(job.cancellation.inbox(), Interest::Read));
                if let Some(at) = job.cancellation.deadline() {
                    deadline = deadline.min(at);
                }
            }
            if let Some(at) = job.settlement_retry {
                deadline = deadline.min(at);
            }
        }
        fds.extend_from_slice(extra);
        let ready = if immediate {
            aterm_uds::ready::any_interests_now(&fds)?
        } else {
            aterm_uds::ready::any_interests_until(&fds, deadline)?
        };
        Ok(Wake {
            control_ready: ready.contains(&0),
            jobs_hint: immediate || ready.iter().any(|&i| i != 0),
            deadline: if immediate {
                Instant::now() >= deadline
            } else {
                ready.is_empty()
            },
        })
    }
}

struct NativeJob {
    held: Option<Held>,
    owned: Option<SupervisedChild>,
    meter: Option<TreeMeter>,
    cancellation: Cancellation,
    starting: bool,
    stopped: bool,
    cleanup_issued: bool,
    usage: Option<Rusage>,
    failure: Option<io::Error>,
    retained: Option<Retained>,
    settled: bool,
    settlement_retry: Option<Instant>,
    settlement_failure: Option<io::Error>,
}

impl NativeJob {
    fn dispatch(&mut self, command: &mut spawnfd::Command) {
        self.dispatch_with(command, |held, command| launch_held(Some(held), command));
    }

    fn dispatch_with(
        &mut self,
        command: &mut spawnfd::Command,
        launch: impl FnOnce(
            &mut Held,
            &mut spawnfd::Command,
        ) -> io::Result<(SupervisedChild, Option<io::Error>)>,
    ) {
        command.env(
            aterm_capacity::COVER_ENV,
            self.held
                .as_ref()
                .expect("owned admission")
                .claim()
                .record()
                .cover_marker(),
        );
        struct Restore<'a>(&'a mut spawnfd::Command, bool);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.start_suspended(self.1);
            }
        }
        let configured = command.starts_suspended();
        command.start_suspended(true);
        let restore = Restore(command, configured);
        match launch(self.held.as_mut().expect("owned admission"), restore.0) {
            Ok((owned, error)) => {
                self.owned = Some(owned);
                self.failure = error;
                self.meter = Some(TreeMeter::start(
                    self.owned.as_mut().expect("owned child").child_mut().id(),
                ));
            }
            Err(error) => {
                self.starting = false;
                self.failure = Some(error);
            }
        }
    }

    fn settle_retained(&mut self, now: Instant) -> bool {
        self.settle_retained_with(now, |held| held.claim.finish_cohort())
    }

    fn settle_retained_with(
        &mut self,
        now: Instant,
        settle: impl FnOnce(&mut Held) -> io::Result<()>,
    ) -> bool {
        if self.settled {
            return true;
        }
        if self.owned.is_some() && self.usage.is_none() {
            return false;
        }
        if self.settlement_retry.is_some_and(|at| now < at) {
            return false;
        }
        // No child handle is not a no-child proof. The original claim must itself
        // replay any exact cancellation/BIND and establish native scope drain.
        match settle(self.held.as_mut().expect("owned admission")) {
            Ok(()) => {
                self.settled = true;
                self.settlement_failure = None;
            }
            Err(error) => self.settlement_failure = Some(error),
        }
        self.settlement_retry = (!self.settled).then(|| now + SETTLEMENT_RETRY);
        self.settled
    }

    fn poll_retained_cleanup(&mut self) -> bool {
        self.poll_retained_cleanup_with(|child| child.try_wait_with_rusage())
    }

    fn poll_retained_cleanup_with(
        &mut self,
        poll: impl FnOnce(&mut spawnfd::Child) -> io::Result<Option<Rusage>>,
    ) -> bool {
        if let Some(owned) = self.owned.as_mut().filter(|_| self.usage.is_none()) {
            if !self.cleanup_issued {
                self.cleanup_issued = request_owned_cleanup(owned, &mut |_| {});
            }
            if self.cleanup_issued
                && let Ok(Some(usage)) = poll(owned.child_mut())
            {
                self.usage = Some(usage);
            }
        }
        self.settle_retained(Instant::now())
    }

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
        let Some(owned) = self.owned.as_mut().filter(|_| self.usage.is_none()) else {
            return Ok(());
        };
        if self.starting {
            let Some(observed) = owned.child_mut().try_job_event()? else {
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
            self.cancellation.service(owned, events)?;
            if !self.cancellation.requested() {
                owned.child_mut().resume()?;
            }
            return Ok(());
        }
        self.cancellation.service(owned, events)?;
        self.observe(events, true)
    }

    fn observe(&mut self, events: &mut JobEvents<'_>, decisions: bool) -> io::Result<()> {
        if self.cancellation.exit_observed() && self.cancellation.holds_root() && decisions {
            return Ok(());
        }
        let owned = self.owned.as_mut().expect("observe an actual child");
        let Some(observed) = owned.child_mut().try_job_event()? else {
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
                        owned.request_group(CohortSignal::Continue)?;
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
                    JobControl::TerminateGroup => self.cancellation.terminate(owned, events),
                    JobControl::KillGroup => self.cancellation.kill(owned, events),
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
    ) -> (io::Result<JobOutcome>, Option<returning::Returned>) {
        let result = self.finish_outcome(events, say);
        let returned = self
            .retained
            .take()
            .and_then(|resources| resources.complete(result.as_ref().ok().copied()));
        (result, returned)
    }

    fn finish_outcome(
        &mut self,
        events: &mut JobEvents<'_>,
        say: &mut dyn FnMut(&str),
    ) -> io::Result<JobOutcome> {
        let mut held = self.held.take().expect("owned admission");
        let settlement = match if self.settled {
            Ok(())
        } else {
            held.claim.finish_cohort()
        } {
            Ok(()) => CohortSettlement::Drained,
            Err(error) => {
                say(&format!(
                    "build-admit: host job reservation retained ({error})"
                ));
                CohortSettlement::Retained
            }
        };
        let Some(usage) = self.usage else {
            debug_assert!(self.retained.is_some() && self.settled);
            drop(held);
            return Err(self
                .failure
                .take()
                .unwrap_or_else(|| io::Error::other("retained attempt started no root")));
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
        let whole = self.failure.is_none()
            && !outcome.cancelled
            && outcome.settlement == Some(CohortSettlement::Drained);
        held.let_go().keep(
            super::BuildEnd::judged(Some(usage.status), whole),
            peak,
            say,
        );
        self.failure.take().map_or(Ok(outcome), Err)
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        if self.owner != std::process::id() {
            // A foreign process copy cannot release arbitrary guards belonging to
            // the original owner, or hand its copied child receipts to a reaper.
            for (_, job) in std::mem::take(&mut self.jobs) {
                if job.retained.is_some() {
                    std::mem::forget(job);
                }
            }
            return;
        }
        for job in self.jobs.values_mut() {
            if let Some(owned) = job.owned.as_mut()
                && !owned.child_mut().is_reaped()
            {
                job.cleanup_issued |= request_owned_cleanup(owned, &mut |_| {});
            }
        }
        for (_, mut job) in std::mem::take(&mut self.jobs) {
            if job.retained.is_some() {
                aterm_uds::reaper::hand_off(move || job.poll_retained_cleanup());
            } else if job.cleanup_issued {
                let _ = job.owned.as_mut().expect("legacy child").child_mut().wait();
            }
        }
    }
}

#[cfg(test)]
#[path = "build_admit_retained_tests.rs"]
mod retained_tests;
