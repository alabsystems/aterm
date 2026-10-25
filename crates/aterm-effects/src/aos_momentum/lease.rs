// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lease ledger of Aos M5b's distributed search (design §11.5): which worker holds which
//! job, on the coordinator's clock alone. Pure: every call is given `now`, and the ledger sees a
//! claim only as its digest.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use aterm_aos::Digest;

/// Nanoseconds since the coordinator's origin.
pub type Nanos = u64;

/// A worker, by the order it joined in; ids are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerId(pub u32);

/// A lease: one attempt of one job of one search, held by one worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Token {
    /// The command's search serial.
    pub search: u32,
    /// The job's ordinal in its search.
    pub slot: u32,
    /// The attempt, from 1.
    pub attempt: u8,
    /// The holder.
    pub worker: u32,
}

/// The ledger's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most workers that may join.
    pub max_workers: usize,
    /// How long a worker may take to say hello.
    pub hello_ns: Nanos,
    /// How long a lease lives without progress.
    pub window_ns: Nanos,
    /// How long a worker sent `Cancel` may take to stop.
    pub grace_ns: Nanos,
    /// The most leases one job is given.
    pub max_attempts: u8,
    /// The training items a job judges: progress names one of `0..items`.
    pub items: u16,
}

/// The bounds of design §11.5 (`max_workers` is the coordinator's, at most 16).
pub const LIMITS_V1: Limits = Limits {
    max_workers: 16,
    hello_ns: 60_000_000_000,
    window_ns: 60_000_000_000,
    grace_ns: 5_000_000_000,
    max_attempts: 3,
    items: 17,
};

/// Why a worker left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gone {
    /// Its pipe closed or it exited.
    Eof,
    /// It did not say hello in time.
    HelloTimeout,
    /// It did not stop within the grace after `Cancel`.
    DrainTimeout,
    /// Its hello named another execution identity: the first field that differs.
    Foreign(&'static str),
    /// It stopped after `Cancel`, or sent a late result, and takes no new lease.
    Retired,
    /// It sent a frame that voided the search.
    Refused,
}

/// Why a worker is draining.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Drain {
    /// Its lease's window passed; the job is open again.
    Expired,
    /// The search was cancelled.
    Cancelled,
}

/// A worker's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkerState {
    /// Spawned, no hello yet.
    Joining {
        /// When it joined.
        since: Nanos,
    },
    /// Ready for a lease.
    Idle,
    /// Holding `lease` until `deadline`, last reporting item `k`.
    Busy {
        /// The lease.
        lease: Token,
        /// When it expires.
        deadline: Nanos,
        /// The last item it reported.
        k: Option<u16>,
    },
    /// Sent `Cancel` for `lease`, and given until `until` to stop.
    Draining {
        /// The lease it held.
        lease: Token,
        /// When the grace ends.
        until: Nanos,
        /// Why.
        why: Drain,
        /// The last item it reported: the `k` rule binds a drain too.
        k: Option<u16>,
        /// Whether it sent the result it had in hand when cancelled (dropped).
        answered: bool,
    },
    /// Left, and killed.
    Gone(Gone),
}

/// A job's state in the current search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobState {
    /// Waiting, after `attempts` leases.
    Pending {
        /// The leases it was given.
        attempts: u8,
    },
    /// Held by `lease`.
    Leased {
        /// The lease.
        lease: Token,
    },
    /// `lease`'s holder returned the claim `digest`.
    Claimed {
        /// The lease.
        lease: Token,
        /// The claim's digest.
        digest: Digest,
    },
    /// The search was cancelled before it was claimed.
    Cancelled,
    /// Its last allowed lease was lost or expired.
    Exhausted,
}

/// Why the ledger voided the search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LeaseRefusal {
    /// A frame other than hello from a worker that has not said hello.
    NotJoined {
        /// The sender.
        worker: WorkerId,
    },
    /// A second hello.
    SecondHello {
        /// The sender.
        worker: WorkerId,
    },
    /// A token never issued to the sender, or not its live, draining or claimed lease.
    NotHeld {
        /// The token sent.
        sent: Token,
    },
    /// Progress from the holder that does not advance, or names no training item.
    NoProgress {
        /// The item named.
        k: u16,
        /// The last item named, if any.
        last: Option<u16>,
    },
    /// A frame nothing asked for: a second resend, progress for a claimed lease, or `Cancelled`
    /// with no `Cancel` sent.
    Unasked {
        /// The token sent.
        sent: Token,
    },
    /// A resend or late result whose claim differs from the one accepted for its job.
    Differs {
        /// The search.
        search: u32,
        /// The job.
        slot: u32,
    },
    /// A job's last allowed lease was lost or expired.
    Exhausted {
        /// The job.
        slot: u32,
    },
    /// No worker is left to run the open jobs.
    NoWorkers {
        /// The jobs not yet claimed.
        pending: u32,
    },
}

impl fmt::Display for LeaseRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseRefusal::NotJoined { worker } => {
                write!(f, "worker {} sent a frame before its hello", worker.0)
            }
            LeaseRefusal::SecondHello { worker } => {
                write!(f, "worker {} said hello twice", worker.0)
            }
            LeaseRefusal::NotHeld { sent } => write!(
                f,
                "a token not held by its sender: search {} slot {} attempt {} worker {}",
                sent.search, sent.slot, sent.attempt, sent.worker
            ),
            LeaseRefusal::NoProgress { k, last } => match last {
                Some(last) => write!(
                    f,
                    "progress at item {k} after item {last}: it does not advance or names no training item"
                ),
                None => write!(f, "progress at item {k}: it names no training item"),
            },
            LeaseRefusal::Unasked { sent } => write!(
                f,
                "a frame nothing asked for, for search {} slot {} attempt {}",
                sent.search, sent.slot, sent.attempt
            ),
            LeaseRefusal::Differs { search, slot } => write!(
                f,
                "a resent or late claim for search {search} slot {slot} differs from the one accepted"
            ),
            LeaseRefusal::Exhausted { slot } => {
                write!(f, "job {slot}'s last allowed lease was lost or expired")
            }
            LeaseRefusal::NoWorkers { pending } => {
                write!(f, "no worker is left for the {pending} open jobs")
            }
        }
    }
}

impl std::error::Error for LeaseRefusal {}

/// Who voided the search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VoidBy {
    /// The ledger, for this reason.
    Ledger(LeaseRefusal),
    /// The caller ([`Ledger::void`]), who keeps the reason.
    Caller,
    /// An interrupt ([`Ledger::void`] by the shell).
    Interrupted,
}

/// Where the command's distributed work stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// No search is open, none was void.
    Idle,
    /// The current search has open jobs.
    Open,
    /// The current search is complete: every job claimed, no worker draining.
    Complete,
    /// Void: the command's distributed work ends.
    Void(VoidBy),
}

/// What the caller must do after a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Send `Lease` for `token` to the worker.
    Lease(WorkerId, Token),
    /// Send `Cancel` for `token` to the worker.
    Cancel(WorkerId, Token),
    /// Kill the worker and stop reading it.
    Kill(WorkerId),
    /// The worker's result for `token` was accepted: keep its claim.
    Accepted(WorkerId, Token),
}

/// A call made with an earlier `now` than one already given; it changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClockBackwards {
    /// The latest `now` given.
    pub latest: Nanos,
    /// The `now` refused.
    pub given: Nanos,
}

impl fmt::Display for ClockBackwards {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the clock ran backwards: {} ns after {} ns",
            self.given, self.latest
        )
    }
}

impl std::error::Error for ClockBackwards {}

/// What the ledger counted, for the summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Counts {
    /// Workers that joined.
    pub joined: u64,
    /// Leases issued.
    pub leases: u64,
    /// Leases after a job's first: retries.
    pub retries: u64,
    /// Leases that expired.
    pub expired: u64,
    /// Workers lost: EOF, hello timeout or drain timeout.
    pub lost: u64,
    /// Workers refused for a foreign hello.
    pub foreign: u64,
    /// `Cancelled` acknowledgements.
    pub cancelled: u64,
    /// Resends equal to the accepted claim.
    pub duplicate: u64,
    /// Late results equal to the accepted claim.
    pub stale: u64,
    /// Progress from a draining worker for its lease, dropped.
    pub late_progress: u64,
    /// Frames from gone workers, or results from workers draining after a cancel, dropped.
    pub dropped: u64,
}

/// The lease ledger.
#[derive(Clone, Debug)]
pub struct Ledger {
    limits: Limits,
    latest: Nanos,
    workers: Vec<WorkerState>,
    /// The current search's serial (0 before the first).
    search: u32,
    jobs: Vec<JobState>,
    open: bool,
    /// Every claimed token of every finished search, with its digest.
    finished: BTreeMap<Token, Digest>,
    /// Tokens already resent once.
    resent: BTreeSet<Token>,
    /// Late results of the current search awaiting their job's claim, by slot.
    stale: BTreeMap<u32, Vec<Digest>>,
    void: Option<VoidBy>,
    counts: Counts,
}

impl Ledger {
    /// An empty ledger under `limits`.
    #[must_use]
    pub const fn new(limits: Limits) -> Ledger {
        Ledger {
            limits,
            latest: 0,
            workers: Vec::new(),
            search: 0,
            jobs: Vec::new(),
            open: false,
            finished: BTreeMap::new(),
            resent: BTreeSet::new(),
            stale: BTreeMap::new(),
            void: None,
            counts: Counts {
                joined: 0,
                leases: 0,
                retries: 0,
                expired: 0,
                lost: 0,
                foreign: 0,
                cancelled: 0,
                duplicate: 0,
                stale: 0,
                late_progress: 0,
                dropped: 0,
            },
        }
    }

    /// Its bounds.
    #[inline]
    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// What it counted.
    #[inline]
    #[must_use]
    pub const fn counts(&self) -> Counts {
        self.counts
    }

    /// The current search's serial.
    #[inline]
    #[must_use]
    pub const fn search(&self) -> u32 {
        self.search
    }

    /// Where the work stands.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        match self.void {
            Some(by) => Outcome::Void(by),
            None if self.open => Outcome::Open,
            None if self.search == 0 => Outcome::Idle,
            None => Outcome::Complete,
        }
    }

    /// Worker `w`'s state.
    #[must_use]
    pub const fn worker(&self, w: WorkerId) -> Option<WorkerState> {
        let workers = self.workers.as_slice();
        let idx = index(w.0);
        if idx < workers.len() {
            Some(workers[idx])
        } else {
            None
        }
    }

    /// Every worker's state, by id.
    #[must_use]
    pub fn workers(&self) -> &[WorkerState] {
        &self.workers
    }

    /// The current search's job `slot`.
    #[must_use]
    pub const fn job(&self, slot: u32) -> Option<JobState> {
        let jobs = self.jobs.as_slice();
        let idx = index(slot);
        if idx < jobs.len() {
            Some(jobs[idx])
        } else {
            None
        }
    }

    /// The current search's jobs, by slot.
    #[must_use]
    pub fn jobs(&self) -> &[JobState] {
        &self.jobs
    }

    /// The late results of the current search's job `slot` that wait for its claim, to be
    /// compared with it when it is accepted (design §11.5).
    #[must_use]
    pub fn awaiting(&self, slot: u32) -> &[Digest] {
        self.stale.get(&slot).map_or(&[], Vec::as_slice)
    }

    /// Whether no worker holds or is draining a lease.
    #[must_use]
    pub fn quiescent(&self) -> bool {
        let workers = self.workers.as_slice();
        let mut i = 0;
        while i < workers.len() {
            if matches!(
                workers[i],
                WorkerState::Busy { .. } | WorkerState::Draining { .. }
            ) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The earliest time a [`Self::tick`] can change anything.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Nanos> {
        let workers = self.workers.as_slice();
        let mut best: Option<Nanos> = None;
        let mut i = 0;
        while i < workers.len() {
            let cand = match workers[i] {
                WorkerState::Joining { since } => Some(since.saturating_add(self.limits.hello_ns)),
                WorkerState::Busy { deadline, .. } => Some(deadline),
                WorkerState::Draining { until, .. } => Some(until),
                WorkerState::Idle | WorkerState::Gone(_) => None,
            };
            if let Some(d) = cand {
                best = Some(match best {
                    Some(prev) if prev <= d => prev,
                    _ => d,
                });
            }
            i += 1;
        }
        best
    }

    const fn clock(&mut self, now: Nanos) -> Result<(), ClockBackwards> {
        if now < self.latest {
            return Err(ClockBackwards {
                latest: self.latest,
                given: now,
            });
        }
        self.latest = now;
        Ok(())
    }

    fn set_worker(&mut self, w: WorkerId, s: WorkerState) {
        if let Some(slot) = self.workers.get_mut(index(w.0)) {
            *slot = s;
        }
    }

    fn set_job(&mut self, slot: u32, s: JobState) {
        if let Some(j) = self.jobs.get_mut(index(slot)) {
            *j = s;
        }
    }

    /// `w` gone for `why`, and killed.
    fn gone(&mut self, w: WorkerId, why: Gone, out: &mut Vec<Effect>) {
        self.set_worker(w, WorkerState::Gone(why));
        match why {
            Gone::Eof | Gone::HelloTimeout | Gone::DrainTimeout => self.counts.lost += 1,
            Gone::Foreign(_) => self.counts.foreign += 1,
            Gone::Retired | Gone::Refused => {}
        }
        out.push(Effect::Kill(w));
    }

    /// The job `lease` held is open again, or exhausted after its last allowed lease.
    fn release(&mut self, lease: Token) {
        if lease.search != self.search || self.job(lease.slot) != Some(JobState::Leased { lease }) {
            return;
        }
        if lease.attempt >= self.limits.max_attempts {
            self.set_job(lease.slot, JobState::Exhausted);
        } else {
            self.set_job(
                lease.slot,
                JobState::Pending {
                    attempts: lease.attempt,
                },
            );
        }
    }

    /// Voids the work for `by` (once), cancelling every open job and holder.
    fn void_with(&mut self, by: VoidBy, now: Nanos, out: &mut Vec<Effect>) {
        if self.void.is_some() {
            return;
        }
        self.void = Some(by);
        self.cancel_jobs(now, out);
    }

    fn cancel_jobs(&mut self, now: Nanos, out: &mut Vec<Effect>) {
        for j in &mut self.jobs {
            if matches!(j, JobState::Pending { .. } | JobState::Leased { .. }) {
                *j = JobState::Cancelled;
            }
        }
        let until = now.saturating_add(self.limits.grace_ns);
        for (i, s) in self.workers.iter_mut().enumerate() {
            if let WorkerState::Busy { lease, k, .. } = *s {
                *s = WorkerState::Draining {
                    lease,
                    until,
                    why: Drain::Cancelled,
                    k,
                    answered: false,
                };
                out.push(Effect::Cancel(WorkerId(wid(i)), lease));
            }
        }
    }

    /// After every call: exhaustion or no workers.
    fn settle(&mut self, now: Nanos, out: &mut Vec<Effect>) {
        if !self.open || self.void.is_some() {
            return;
        }
        let mut pending = 0usize;
        for (slot, j) in self.jobs.iter().enumerate() {
            match j {
                JobState::Exhausted => {
                    let refusal = LeaseRefusal::Exhausted { slot: wid(slot) };
                    self.void_with(VoidBy::Ledger(refusal), now, out);
                    return;
                }
                JobState::Pending { .. } | JobState::Leased { .. } => {
                    pending += 1;
                }
                JobState::Claimed { .. } | JobState::Cancelled => {}
            }
        }
        if pending > 0 {
            let mut able = false;
            for s in &self.workers {
                if matches!(
                    s,
                    WorkerState::Joining { .. } | WorkerState::Idle | WorkerState::Busy { .. }
                ) {
                    able = true;
                    break;
                }
            }
            if !able {
                let refusal = LeaseRefusal::NoWorkers {
                    pending: wid(pending),
                };
                self.void_with(VoidBy::Ledger(refusal), now, out);
            }
        }
    }

    /// Completes the open search when every job is claimed and no worker is draining (design
    /// §11.5); otherwise changes nothing. Design §11.3's step 5 runs only after it.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn finish(&mut self, now: Nanos) -> Result<(), ClockBackwards> {
        self.clock(now)?;
        if !self.open || self.void.is_some() {
            return Ok(());
        }
        for j in &self.jobs {
            if !matches!(j, JobState::Claimed { .. }) {
                return Ok(());
            }
        }
        for s in &self.workers {
            if matches!(s, WorkerState::Draining { .. }) {
                return Ok(());
            }
        }
        self.open = false;
        for j in &self.jobs {
            if let JobState::Claimed { lease, digest } = *j {
                self.finished.insert(lease, digest);
            }
        }
        self.stale.clear();
        Ok(())
    }

    /// `w`'s frame voids the search for `by`: `w` is killed first, so it is sent no `Cancel`.
    fn refuse_from(&mut self, w: WorkerId, by: VoidBy, now: Nanos, out: &mut Vec<Effect>) {
        if !matches!(self.worker(w), Some(WorkerState::Gone(_)) | None) {
            self.gone(w, Gone::Refused, out);
        }
        self.void_with(by, now, out);
    }

    fn refuse(&mut self, w: WorkerId, r: LeaseRefusal, now: Nanos, out: &mut Vec<Effect>) {
        self.refuse_from(w, VoidBy::Ledger(r), now, out);
    }

    /// A new worker, `Joining`; `None` past [`Limits::max_workers`].
    /// # Errors
    /// [`ClockBackwards`].
    pub fn join(&mut self, now: Nanos) -> Result<Option<WorkerId>, ClockBackwards> {
        self.clock(now)?;
        if self.workers.len() >= self.limits.max_workers {
            return Ok(None);
        }
        let w = WorkerId(wid(self.workers.len()));
        self.workers.push(WorkerState::Joining { since: now });
        self.counts.joined += 1;
        Ok(Some(w))
    }

    /// Opens search `search` with `jobs` jobs, each pending ([`Self::assign`] leases them).
    /// Nothing happens while a search is open, after a void, or for a serial not above the last.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn begin(
        &mut self,
        search: u32,
        jobs: u32,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        if self.open || self.void.is_some() || search <= self.search {
            return Ok(out);
        }
        self.search = search;
        self.jobs = vec![JobState::Pending { attempts: 0 }; index(jobs)];
        self.open = true;
        self.stale.clear();
        self.settle(now, &mut out);
        Ok(out)
    }

    /// `w`'s hello: `Ok` when every identity field matched, else the first that differs.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn hello(
        &mut self,
        w: WorkerId,
        matched: Result<(), &'static str>,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        match self.worker(w) {
            Some(WorkerState::Joining { .. }) => match matched {
                Ok(()) => self.set_worker(w, WorkerState::Idle),
                Err(field) => self.gone(w, Gone::Foreign(field), &mut out),
            },
            Some(WorkerState::Gone(_)) | None => self.counts.dropped += 1,
            Some(_) => self.refuse(w, LeaseRefusal::SecondHello { worker: w }, now, &mut out),
        }
        self.settle(now, &mut out);
        Ok(out)
    }

    /// Leases `w`, if it is idle, the lowest pending job of the open search.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn lease(&mut self, w: WorkerId, now: Nanos) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        self.lease_into(w, now, &mut out);
        self.settle(now, &mut out);
        Ok(out)
    }

    /// Leases every idle worker, in id order, the lowest pending jobs: what the coordinator does
    /// after every event.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn assign(&mut self, now: Nanos) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        self.assign_into(now, &mut out);
        self.settle(now, &mut out);
        Ok(out)
    }

    fn lease_into(&mut self, w: WorkerId, now: Nanos, out: &mut Vec<Effect>) {
        if !self.open || self.void.is_some() || self.worker(w) != Some(WorkerState::Idle) {
            return;
        }
        let mut found = None;
        for (i, j) in self.jobs.iter().enumerate() {
            if let JobState::Pending { attempts } = *j {
                found = Some((i, attempts));
                break;
            }
        }
        let Some((slot, attempts)) = found else {
            return;
        };
        let lease = Token {
            search: self.search,
            slot: wid(slot),
            attempt: attempts.saturating_add(1),
            worker: w.0,
        };
        self.set_job(lease.slot, JobState::Leased { lease });
        self.set_worker(
            w,
            WorkerState::Busy {
                lease,
                deadline: now.saturating_add(self.limits.window_ns),
                k: None,
            },
        );
        self.counts.leases += 1;
        if attempts > 0 {
            self.counts.retries += 1;
        }
        out.push(Effect::Lease(w, lease));
    }

    fn assign_into(&mut self, now: Nanos, out: &mut Vec<Effect>) {
        for i in 0..self.workers.len() {
            self.lease_into(WorkerId(wid(i)), now, out);
        }
    }

    /// Whether `sent` is a claimed lease of its sender `w`, in this search or a finished one,
    /// and that claim's digest.
    fn claimed_by(&self, w: WorkerId, sent: Token) -> Option<Digest> {
        if sent.worker != w.0 {
            return None;
        }
        if sent.search == self.search
            && let Some(JobState::Claimed { lease, digest }) = self.job(sent.slot)
            && lease == sent
        {
            return Some(digest);
        }
        self.finished.get(&sent).copied()
    }

    /// A frame from a gone worker, dropped unread.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn drop_unread(&mut self, now: Nanos) -> Result<(), ClockBackwards> {
        self.clock(now)?;
        self.counts.dropped += 1;
        Ok(())
    }

    /// `w` reports item `k` of `sent` judged.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn progress(
        &mut self,
        w: WorkerId,
        sent: Token,
        k: u16,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        match self.worker(w) {
            Some(WorkerState::Gone(_)) | None => self.counts.dropped += 1,
            Some(WorkerState::Joining { .. }) => {
                self.refuse(w, LeaseRefusal::NotJoined { worker: w }, now, &mut out);
            }
            Some(WorkerState::Busy { lease, k: last, .. }) if lease == sent => {
                if k < self.limits.items && last.is_none_or(|last| k > last) {
                    self.set_worker(
                        w,
                        WorkerState::Busy {
                            lease,
                            deadline: now.saturating_add(self.limits.window_ns),
                            k: Some(k),
                        },
                    );
                } else {
                    self.refuse(w, LeaseRefusal::NoProgress { k, last }, now, &mut out);
                }
            }
            Some(WorkerState::Draining {
                lease,
                until,
                why,
                k: last,
                answered,
            }) if lease == sent => {
                if k < self.limits.items && last.is_none_or(|last| k > last) {
                    self.set_worker(
                        w,
                        WorkerState::Draining {
                            lease,
                            until,
                            why,
                            k: Some(k),
                            answered,
                        },
                    );
                    self.counts.late_progress += 1;
                } else {
                    self.refuse(w, LeaseRefusal::NoProgress { k, last }, now, &mut out);
                }
            }
            Some(_) if self.claimed_by(w, sent).is_some() => {
                self.refuse(w, LeaseRefusal::Unasked { sent }, now, &mut out);
            }
            Some(_) => self.refuse(w, LeaseRefusal::NotHeld { sent }, now, &mut out),
        }
        self.settle(now, &mut out);
        Ok(out)
    }

    /// `w` returns the claim `digest` for `sent`.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn result(
        &mut self,
        w: WorkerId,
        sent: Token,
        digest: Digest,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        match self.worker(w) {
            Some(WorkerState::Gone(_)) | None => self.counts.dropped += 1,
            Some(WorkerState::Joining { .. }) => {
                self.refuse(w, LeaseRefusal::NotJoined { worker: w }, now, &mut out);
            }
            Some(WorkerState::Busy { lease, .. }) if lease == sent => {
                self.set_job(lease.slot, JobState::Claimed { lease, digest });
                self.set_worker(w, WorkerState::Idle);
                out.push(Effect::Accepted(w, lease));
                let waiting = self.stale.remove(&lease.slot).unwrap_or_default();
                self.compare_stale(lease, digest, &waiting, now, &mut out);
            }
            Some(WorkerState::Draining {
                lease,
                why: Drain::Expired,
                ..
            }) if lease == sent => {
                self.gone(w, Gone::Retired, &mut out);
                match self.job(lease.slot) {
                    Some(JobState::Claimed {
                        lease: accepted,
                        digest: theirs,
                    }) => self.compare_stale(accepted, theirs, &[digest], now, &mut out),
                    Some(JobState::Pending { .. } | JobState::Leased { .. }) => {
                        self.stale.entry(lease.slot).or_default().push(digest);
                    }
                    Some(JobState::Cancelled | JobState::Exhausted) | None => {
                        self.counts.dropped += 1;
                    }
                }
            }
            Some(WorkerState::Draining {
                lease,
                until,
                why: Drain::Cancelled,
                k,
                answered: false,
            }) if lease == sent => {
                self.set_worker(
                    w,
                    WorkerState::Draining {
                        lease,
                        until,
                        why: Drain::Cancelled,
                        k,
                        answered: true,
                    },
                );
                self.counts.dropped += 1;
            }
            Some(WorkerState::Draining {
                lease,
                answered: true,
                ..
            }) if lease == sent => self.refuse(w, LeaseRefusal::Unasked { sent }, now, &mut out),
            Some(_) => match self.claimed_by(w, sent) {
                Some(_) if !self.resent.insert(sent) => {
                    self.refuse(w, LeaseRefusal::Unasked { sent }, now, &mut out);
                }
                Some(accepted) if accepted == digest => self.counts.duplicate += 1,
                Some(_) => self.refuse(
                    w,
                    LeaseRefusal::Differs {
                        search: sent.search,
                        slot: sent.slot,
                    },
                    now,
                    &mut out,
                ),
                None => self.refuse(w, LeaseRefusal::NotHeld { sent }, now, &mut out),
            },
        }
        self.settle(now, &mut out);
        Ok(out)
    }

    fn compare_stale(
        &mut self,
        accepted: Token,
        digest: Digest,
        late: &[Digest],
        now: Nanos,
        out: &mut Vec<Effect>,
    ) {
        for d in late {
            if *d == digest {
                self.counts.stale += 1;
            } else {
                let refusal = LeaseRefusal::Differs {
                    search: accepted.search,
                    slot: accepted.slot,
                };
                self.void_with(VoidBy::Ledger(refusal), now, out);
                return;
            }
        }
    }

    /// `w` acknowledges `Cancel` for `sent`.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn cancelled(
        &mut self,
        w: WorkerId,
        sent: Token,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        match self.worker(w) {
            Some(WorkerState::Gone(_)) | None => self.counts.dropped += 1,
            Some(WorkerState::Joining { .. }) => {
                self.refuse(w, LeaseRefusal::NotJoined { worker: w }, now, &mut out);
            }
            Some(WorkerState::Draining { lease, .. }) if lease == sent => {
                self.counts.cancelled += 1;
                self.gone(w, Gone::Retired, &mut out);
            }
            Some(_) => self.refuse(w, LeaseRefusal::Unasked { sent }, now, &mut out),
        }
        self.settle(now, &mut out);
        Ok(out)
    }

    /// `w`'s pipe closed or it exited.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn lost(&mut self, w: WorkerId, now: Nanos) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        match self.worker(w) {
            Some(WorkerState::Gone(_)) | None => {}
            Some(WorkerState::Busy { lease, .. }) => {
                self.gone(w, Gone::Eof, &mut out);
                self.release(lease);
            }
            Some(_) => self.gone(w, Gone::Eof, &mut out),
        }
        self.settle(now, &mut out);
        Ok(out)
    }

    /// Expires what is due at `now`: hellos, leases and drains.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn tick(&mut self, now: Nanos) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        for i in 0..self.workers.len() {
            let w = WorkerId(wid(i));
            match self.worker(w) {
                Some(WorkerState::Joining { since })
                    if now >= since.saturating_add(self.limits.hello_ns) =>
                {
                    self.gone(w, Gone::HelloTimeout, &mut out);
                }
                Some(WorkerState::Busy {
                    lease, deadline, k, ..
                }) if now >= deadline => {
                    self.counts.expired += 1;
                    self.release(lease);
                    self.set_worker(
                        w,
                        WorkerState::Draining {
                            lease,
                            until: now.saturating_add(self.limits.grace_ns),
                            why: Drain::Expired,
                            k,
                            answered: false,
                        },
                    );
                    out.push(Effect::Cancel(w, lease));
                }
                Some(WorkerState::Draining { until, .. }) if now >= until => {
                    self.gone(w, Gone::DrainTimeout, &mut out);
                }
                _ => {}
            }
        }
        self.settle(now, &mut out);
        Ok(out)
    }

    /// Voids the work for `by`, a fault the caller found in `w`'s frame: `w` is killed, then
    /// every open job and holder cancelled.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn void_from(
        &mut self,
        w: WorkerId,
        by: VoidBy,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        self.refuse_from(w, by, now, &mut out);
        Ok(out)
    }

    /// Voids the work for `by` (a fault the caller found, or an interrupt), cancelling every open
    /// job and holder; a second void changes nothing.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn void(&mut self, by: VoidBy, now: Nanos) -> Result<Vec<Effect>, ClockBackwards> {
        self.clock(now)?;
        let mut out = Vec::new();
        self.void_with(by, now, &mut out);
        Ok(out)
    }
}

#[inline]
const fn index(n: u32) -> usize {
    n as usize
}

#[inline]
const fn wid(n: usize) -> u32 {
    if n <= u32::MAX as usize {
        n as u32
    } else {
        u32::MAX
    }
}
