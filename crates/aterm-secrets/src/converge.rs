// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Starting the converge pass's runner (`aterm init --converge`, `aterm_machines::sync`), the
//! pass that keeps a Mac aligned with the lab's expected state after an update, a network move
//! or half an hour (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5).
//!
//! Three places start it, each naming itself with `--trigger`: the vault's subscriber (a
//! launchd job: as it starts, which is the moment after an update replaced its program, after
//! each version it applies, and in a healthy round once [`Due`] says so), the aterm window's
//! lane, and the first-launch Secrets stage as it ends. Each starts [`runner`] in a session of
//! its own and never disclaims responsibility for it, so macOS keeps attributing the runner's
//! privacy-gated acts to the app that started it. A subscriber whose job runs a held copy (an
//! admin Mac's snapshot, a program `agent on --program` named) starts that copy, and the copy
//! hands the pass to the installed aterm.app (`aterm init --converge`'s hand-off), started as its
//! own responsible process: the rows it applies are aterm.app's, as the window's are.
//!
//! A runner a starter started never ends for the lock another pass holds: it waits for it in
//! the kernel and runs once it is let go (`aterm_machines::sync::wait_lock`), running nothing
//! only where its starter ended first (a restarted subscriber, a quit window), whose successor
//! starts its own. A held copy's hand-off keeps that true one process on: the installed app's
//! runner watches the copy that started it, and the copy ends as soon as the subscriber that
//! started it does. Each starter keeps one runner at a time, so what comes due while its runner
//! waits is owed to the start that follows that runner's end, and no clock starts a runner while
//! the lock is held.

use crate::client::SyncOutcome;
use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::net::IpAddr;
use std::path::Path;

mod note;
mod refresh;
pub use note::{Causes, MarkMoved, NOTE_MAX, START_NOTE_ENV, StartNote, note_word, noting};
pub use refresh::{
    RefreshObservation, RefreshRequest, RefreshScope, RefreshVersion, RepositoryRequest,
};

/// The exit status of a pass that ran nothing for the lock another pass holds: a person's
/// `aterm init --converge`, which never waits for it, or a runner a starter started whose starter
/// ended while it waited (`aterm_machines::sync::Stop::Abandoned`). No starter reads it apart
/// from any other unsuccessful exit ([`Last::of`]): a runner it started waits for the lock.
pub const HELD: u8 = 75;

/// The runner ran without failure but admitted work still waits on a precondition or readback.
pub const UNFINISHED: u8 = 76;

/// A pass has unfinished work whose standing cause requires an input change.
/// Its starter keeps the work owed without scheduling a retry on the clock.
pub const STANDING: u8 = 78;

/// The vault subscriber's `--trigger` word (`aterm_machines::sync::Trigger::Subscriber`).
pub const SUBSCRIBER: &str = "subscriber";

/// The aterm window's lane's `--trigger` word (`aterm_machines::sync::Trigger::Window`).
pub const WINDOW: &str = "window";

/// The first-launch Secrets stage's `--trigger` word (`aterm_machines::sync::Trigger::Setup`).
pub const SETUP: &str = "setup";

/// Requested repository work, never authority over a source, checkout or effect.
/// Coalescing keeps the stronger request while one runner owns the job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RepositoryWork {
    /// Repair local readiness and missing work without forcing a remote refresh.
    #[default]
    Repair,
    /// Also refresh eligible clean checkouts against their admitted origins.
    Refresh,
}

impl RepositoryWork {
    /// The runner's `--repository-work` argument.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::Refresh => "refresh",
        }
    }
}

/// How long after it started a runner the subscriber starts another on the same routes: the
/// pass's own recheck (`aterm_machines::sync::RECHECK`).
pub const RECHECK_MS: u64 = 30 * 60 * 1000;

/// Retry an unfinished or failed runner after five seconds, backing off to five minutes.
pub const FIRST_RETRY_MS: u64 = 5 * 1000;
/// The maximum runner retry delay on unchanged inputs.
pub const LONGEST_RETRY_MS: u64 = 5 * 60 * 1000;

/// The runner for `trigger` (`subscriber`, `window` or `setup`): `exe init --converge --trigger
/// <trigger> --repository-work <work>`, reading and writing nothing of its starter's (stdio null), in a session of its
/// own (`setsid`), so a launchd job's restart, which ends its process group, or a terminal's
/// hangup does not end a pass midway. The [`SUBSCRIBER`]'s runner starts out of its starter's
/// background band ([`aterm_uds::spawnfd::Command::leave_background_band`]): the vault
/// subscriber starts ordinary children in that band, where every thread runs at priority 4
/// whatever class it asks for. The pass writes the fleet book under its lock, whose taker is
/// lifted to the default class while it holds it, a lift that band defeats. The runner holds its own
/// threads at the background class instead, its children started in the band (`aterm init
/// --converge`), so only the lock's holder is lifted. A window's or the first launch's runner
/// starts with its starter's process type, in no band, and holds its threads at utility.
/// Responsibility is never disclaimed: the runner's privacy-gated acts stay the starting app's.
#[must_use]
pub fn runner(exe: &Path, trigger: &str, work: RepositoryWork) -> Command {
    runner_argument(exe, trigger, "--repository-work", work.word())
}

/// A runner carrying one retained shared refresh request, including its original boundary.
#[must_use]
pub fn runner_requested(exe: &Path, trigger: &str, request: RepositoryRequest) -> Command {
    let encoded = aterm_json::to_string(&request).expect("repository request serializes");
    runner_argument(exe, trigger, "--repository-request", &encoded)
}

fn runner_argument(exe: &Path, trigger: &str, argument: &str, value: &str) -> Command {
    let mut command = aterm_uds::spawnfd::command(exe);
    command
        .args(["init", "--converge", "--trigger", trigger, argument, value])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .setsid(true)
        .leave_background_band(trigger == SUBSCRIBER);
    // The start's note, where its starter gave one ([`noting`]): one variable no program reads
    // but the runner, which writes it into its own log line.
    if let Some(word) = note::noted() {
        command.env(START_NOTE_ENV, word);
    }
    command
}

/// Start [`runner`] for `trigger` and return at once: the caller waits for it, or drops it to
/// the process's reaper, never blocking on the pass.
///
/// # Errors
/// The runner could not be started.
pub fn start(exe: &Path, trigger: &str, work: RepositoryWork) -> std::io::Result<Child> {
    runner(exe, trigger, work).spawn()
}

/// Start a runner with one already captured request boundary.
///
/// # Errors
/// The runner could not be started.
pub fn start_requested(
    exe: &Path,
    trigger: &str,
    request: RepositoryRequest,
) -> std::io::Result<Child> {
    runner_requested(exe, trigger, request).spawn()
}

/// This Mac's routes as the subscriber compares them (`crate::server::route_sources`).
pub type Routes = [Option<IpAddr>; 3];

/// How the subscriber's last runner stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Last {
    /// It started none.
    None,
    /// It still runs.
    Running,
    /// It ended successfully.
    Ended,
    /// It ran without failure, with admitted work still unfinished ([`UNFINISHED`]).
    Unfinished,
    /// Its unfinished work waits for an input change ([`STANDING`]), not a clock retry.
    Standing,
    /// It exited unsuccessfully (exit [`HELD`] included), or ended by a signal.
    Failed,
    /// It ran nothing: another owner held what it needs, and it is owed again without spending
    /// or resetting its backoff ([`RunnerRetry::reaped`]). The window's in-process keepers (the
    /// deferred-preference follow-up, the keyboard attachment activation) end so; a converge
    /// runner never does: one a starter started waits for the lock instead.
    Held,
}

impl Last {
    /// How `runner` stands now, reaping and taking it once it ended without waiting. A terminal
    /// outcome is delivered once; a failed wait retains custody until a later read can decide.
    pub fn of(runner: &mut Option<Child>) -> Self {
        let Some(child) = runner.as_mut() else {
            return Self::None;
        };
        let last = match child.try_wait() {
            Ok(None) | Err(_) => Self::Running,
            Ok(Some(status)) if status.code() == Some(i32::from(UNFINISHED)) => Self::Unfinished,
            Ok(Some(status)) if status.code() == Some(i32::from(STANDING)) => Self::Standing,
            Ok(Some(status)) if status.success() => Self::Ended,
            Ok(Some(_)) => Self::Failed,
        };
        if last != Self::Running {
            runner.take();
        }
        last
    }
}

/// Unfinished, failed and held attempt pacing shared by the subscriber, mesh node and window lane
/// (their runners) and the window's in-process keepers (their attempts).
#[derive(Debug, Default)]
pub struct RunnerRetry {
    at: Option<u64>,
    delay: u64,
}

impl RunnerRetry {
    /// Keep one terminal result, using elapsed milliseconds on the caller's monotonic clock.
    pub fn reaped(&mut self, last: Last, now_ms: u64) {
        match last {
            Last::Failed | Last::Unfinished => {
                let pause = self.delay.clamp(FIRST_RETRY_MS, LONGEST_RETRY_MS);
                self.at = Some(now_ms.saturating_add(pause));
                self.delay = pause.saturating_mul(2).min(LONGEST_RETRY_MS);
            }
            Last::Ended | Last::Standing => {
                self.at = None;
                self.delay = 0;
            }
            // A held attempt is owed again, without spending or resetting its backoff.
            Last::Held => self.at = Some(now_ms.saturating_add(FIRST_RETRY_MS)),
            Last::None | Last::Running => {}
        }
    }

    /// Whether the kept retry is due on that clock.
    #[must_use]
    pub fn due(&self, now_ms: u64) -> bool {
        self.at.is_some_and(|at| now_ms >= at)
    }

    /// How long until the kept retry, or none once it has started or completed.
    #[must_use]
    pub fn remaining(&self, now_ms: u64) -> Option<std::time::Duration> {
        self.at
            .map(|at| std::time::Duration::from_millis(at.saturating_sub(now_ms)))
    }

    /// A runner started; keep its backoff until a settled completion resets it.
    pub fn started(&mut self) {
        self.at = None;
    }
}

/// When the vault's subscriber starts a runner: as it starts, after each pass that applied a
/// version which moved something a runner reads ([`SyncOutcome::Applied`]'s `moved`: a version
/// that carried only other Macs' news starts none), after a round that took a pass another
/// program's apply left owed in the vault's state (`client::converge_inputs`), after the first
/// healthy pass that follows failed ones (the vault came back, or this Mac came back to it:
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2), and after any other healthy pass once this
/// Mac's routes moved or [`RECHECK_MS`] passed (or the clock went back) since the last start. One
/// runner at a time: a start that comes due while the last one runs, waiting for the lock
/// another pass holds included, is owed to the round after its end, so a version that moved
/// something during a pass is never left to the next recheck. An unfinished or failed runner
/// retries on a bounded backoff; a standing result retains its work without a deadline until
/// an input moves. Every time here is elapsed milliseconds on the subscriber's monotonic clock.
#[derive(Debug, Default)]
pub struct Due {
    /// The last refresh boundary's local start and remaining wait. A shared
    /// acknowledgment preserves its actual remaining time; repairs never postpone it.
    started: Option<(u64, u64)>,
    /// Routes already observed, including a request queued while the lane was held.
    routes: Option<Routes>,
    /// A start came due while the last runner ran (or waited for the lock).
    pending: Option<RepositoryRequest>,
    /// A canonical identity transition is not paced or fulfilled by repository receipts.
    identity_pending: bool,
    /// Whether the one selected child carries identity debt. Its terminal result cannot be
    /// mistaken for the stronger repository retry it may have run beside.
    identity_running: bool,
    /// Independent logical pacing, serviced by the same existing maintenance clock/worker.
    identity_retry: RunnerRetry,
    /// The last chosen run, retained until its terminal result and used by the starter.
    repository_request: RepositoryRequest,
    /// Failed work waits for RunnerRetry, unless new work starts a stronger combined run.
    retry_request: Option<RepositoryRequest>,
    /// One shared receipt sample, read before the corresponding local-input observation.
    observed: RefreshObservation,
    /// A pass failed since the last healthy one ([`Due::failed`]).
    failing: bool,
    /// The owned pass stopped on standing inputs. Neither backoff nor periodic refresh
    /// starts it again until a request, identity transition or route change is observed.
    parked: bool,
    /// The standing pass's work, retained without a clock deadline.
    parked_request: Option<RepositoryRequest>,
    /// Identity debt carried by that pass is still owed when an input wakes it.
    parked_identity: bool,
    /// Automatic work was held. Its next admitted input capture owes fresh readback.
    activation: bool,
    retry: RunnerRetry,
    /// Why the next start is owed: every cause noted since the last start ([`Due::note`]).
    causes: Causes,
    /// What last set [`Self::retry`]'s time, named once that time starts a runner.
    retry_cause: Causes,
    /// The last start's note.
    noted: StartNote,
    /// What moved the inputs' mark, and in how many looks, since the last start
    /// ([`Due::because_moved`]).
    moved: MarkMoved,
    moves: u32,
}

impl Due {
    /// Retain readback while automatic work is explicitly held, without reading inputs.
    pub(crate) fn paused(&mut self) {
        self.activation = true;
    }

    pub(crate) fn activation_pending(&self) -> bool {
        self.activation
    }

    /// When time alone next makes [`Due::poll`] start a runner, on the same clock: the kept
    /// retry, identity's own retry, or the recheck after the last refresh boundary. Every other
    /// start waits on an event its owner hears (a request, a move of routes, a runner's end).
    /// `None` before the first start, which [`Due::at_start`] makes, and while all owed work
    /// waits for an input change.
    #[must_use]
    pub(crate) fn next_at(&self) -> Option<u64> {
        let recheck = self
            .started
            .filter(|_| !self.parked)
            .map(|(at, wait)| at.saturating_add(wait));
        [self.retry.at, self.identity_retry.at, recheck]
            .into_iter()
            .flatten()
            .min()
    }

    /// Whether [`Due::poll`] at `now_ms` would want a start, a runner still running aside: work
    /// requested or owed, or a retry due. An owner whose starts something else holds (the LAN)
    /// reads this to know a held start is waiting.
    #[must_use]
    pub(crate) fn owes_start(&self, now_ms: u64) -> bool {
        self.activation
            || self.pending.is_some()
            || self.identity_pending
            || self.retry.due(now_ms)
            || self.identity_retry.due(now_ms)
    }

    /// Supply the same receipt facts used to bind this round's input transition.
    pub fn observe(&mut self, observed: RefreshObservation) {
        self.observed = observed;
    }

    /// Retain a request while the owned runner or a shared retry still holds the lane.
    pub(crate) fn request(&mut self, request: RepositoryRequest) {
        self.pending = merge_requests(
            merge_requests(self.parked_request.take(), self.pending),
            Some(request),
        );
        self.identity_pending |= std::mem::take(&mut self.parked_identity);
        self.parked = false;
    }

    /// Note `cause` for the next start's note. It changes no pacing.
    pub(crate) fn because(&mut self, cause: Causes) {
        self.causes = self.causes.with(cause);
    }

    /// Note what moved the inputs' mark in `looks` looks that called for work, for the next
    /// start's note. It changes no pacing.
    pub(crate) fn because_moved(&mut self, moved: MarkMoved, looks: u32) {
        self.moved = self.moved.with(moved);
        self.moves = self.moves.saturating_add(looks);
    }

    /// The note of the start the latest pacing method that returned true chose: why it was
    /// owed, and what moved the inputs' mark.
    #[must_use]
    pub fn note(&self) -> StartNote {
        self.noted
    }

    /// Service the same job while another subscriber lane is waiting on HTTP.
    pub(crate) fn poll(&mut self, now_ms: u64, routes: Routes, last: Last) -> bool {
        self.start(None, now_ms, routes, last)
    }

    /// Canonical identity changed. Schedule local convergence without granting naming or
    /// repository authority, forcing remote freshness, or replacing a stronger pending request.
    pub fn identity_changed(&mut self) {
        self.because(Causes::IDENTITY);
        self.identity_pending = true;
        // A newer canonical intent does not inherit the prior intent's failure backoff.
        self.identity_retry = RunnerRetry::default();
    }

    /// Keep custody and capture event boundaries while the LAN prevents a start.
    pub(crate) fn hold(&mut self, now_ms: u64, routes: Routes, last: Last) {
        self.reaped(last, now_ms);
        self.refresh_due(now_ms, routes);
    }

    /// Verified local member admission may have delivered work independently of HTTP.
    pub(crate) fn after_local_progress(
        &mut self,
        work: RepositoryWork,
        now_ms: u64,
        routes: Routes,
        last: Last,
    ) -> bool {
        let request = match work {
            RepositoryWork::Repair => RepositoryRequest::Repair,
            RepositoryWork::Refresh => {
                RepositoryRequest::Refresh(RefreshRequest::after(None, self.observed.version))
            }
        };
        self.because(Causes::LOCAL);
        self.after_request(request, now_ms, routes, last)
    }

    /// Service one input transition with the same pacing and child custody as other triggers.
    pub(crate) fn after_request(
        &mut self,
        request: RepositoryRequest,
        now_ms: u64,
        routes: Routes,
        last: Last,
    ) -> bool {
        self.start(Some(request), now_ms, routes, last)
    }

    /// Whether the subscriber starts a runner as it starts: always, unless one it started still
    /// runs (none can yet), at `now_ms` with `routes`.
    pub fn at_start(&mut self, now_ms: u64, routes: Routes, last: Last) -> bool {
        self.because(Causes::START);
        self.start(
            Some(RepositoryRequest::Refresh(RefreshRequest::ensure(
                None,
                self.observed.version,
            ))),
            now_ms,
            routes,
            last,
        )
    }

    /// Whether the subscriber starts a runner after a healthy pass that ended with `outcome`, at
    /// `now_ms` with `routes`, its last runner standing as `last`.
    pub fn after_pass(
        &mut self,
        outcome: &SyncOutcome,
        now_ms: u64,
        routes: Routes,
        last: Last,
    ) -> bool {
        let applied = matches!(outcome, SyncOutcome::Applied { moved: true, .. });
        let back = std::mem::take(&mut self.failing);
        if applied {
            self.because(Causes::APPLIED);
        }
        if back {
            self.because(Causes::RECOVERED);
        }
        self.start(
            (applied || back).then_some(RepositoryRequest::Repair),
            now_ms,
            routes,
            last,
        )
    }

    /// Local progress while the source remains unavailable can need a runner,
    /// but cannot consume the trigger for the source's eventual return.
    pub fn after_unreachable_pass(
        &mut self,
        outcome: &SyncOutcome,
        now_ms: u64,
        routes: Routes,
        last: Last,
    ) -> bool {
        self.failed();
        let applied = matches!(outcome, SyncOutcome::Applied { moved: true, .. });
        if applied {
            self.because(Causes::APPLIED);
        }
        self.start(
            applied.then_some(RepositoryRequest::Repair),
            now_ms,
            routes,
            last,
        )
    }

    /// Note a pass that failed: the next healthy one is due ([`Due::after_pass`]).
    pub fn failed(&mut self) {
        self.failing = true;
    }

    /// Keep one reaped runner's outcome even while the vault source is unavailable. Successful
    /// completion ends its failure streak. A Repair that ran beside deferred Refresh debt
    /// cannot retire or reschedule that debt. No runner reads as held ([`Last::of`]); a held end
    /// given here is kept as a failed one, so nothing restarts a runner on a flat clock.
    pub fn reaped(&mut self, last: Last, now_ms: u64) {
        let last = if last == Last::Held {
            Last::Failed
        } else {
            last
        };
        if self.identity_running
            && matches!(
                last,
                Last::Ended | Last::Failed | Last::Unfinished | Last::Standing
            )
        {
            self.parked_identity |= last == Last::Standing;
            self.identity_running = false;
            // A late result retires only the identity that child carried. Newer input stays
            // immediately pending; otherwise its own failure retains a local retry.
            if matches!(last, Last::Ended | Last::Standing) || !self.identity_pending {
                self.identity_retry.reaped(last, now_ms);
            }
        }
        if self.repository_work() == RepositoryWork::Repair
            && self
                .retry_request
                .is_some_and(|request| request.work() == RepositoryWork::Refresh)
        {
            // The stronger retained refresh includes acquisition/repair. A repair
            // that ended cannot create an immediate queue or spend that retry budget.
            return;
        }
        // A started job already took its captured work from the queues. A later
        // success cannot clear retry work that arrived while that job ran.
        if !matches!(last, Last::Ended | Last::Standing) || self.retry_request.is_none() {
            self.retry.reaped(last, now_ms);
            match last {
                Last::Failed => self.retry_cause = Causes::RETRY_FAILED,
                Last::Unfinished => self.retry_cause = Causes::RETRY_UNFINISHED,
                Last::Ended | Last::Standing => self.retry_cause = Causes::default(),
                Last::None | Last::Running | Last::Held => {}
            }
        }
        match last {
            Last::Failed | Last::Unfinished => {
                self.retry_request =
                    merge_requests(Some(self.repository_request), self.retry_request);
            }
            Last::Ended => {
                if let RepositoryRequest::Refresh(request) = self.repository_request
                    && self.observed.fulfills(request)
                {
                    self.acknowledged(now_ms);
                }
            }
            Last::Standing | Last::None | Last::Running | Last::Held => {}
        }
        match last {
            Last::Standing => {
                self.parked = true;
                self.parked_request =
                    merge_requests(self.parked_request, Some(self.repository_request));
            }
            Last::Ended | Last::Failed | Last::Unfinished => self.parked = false,
            Last::None | Last::Running | Last::Held => {}
        }
    }

    /// The work selected by the latest pacing method that returned true.
    #[must_use]
    pub fn repository_work(&self) -> RepositoryWork {
        self.repository_request.work()
    }

    /// The retained request selected by the latest pacing method that returned true.
    #[must_use]
    pub fn repository_request(&self) -> RepositoryRequest {
        self.repository_request
    }

    fn start(
        &mut self,
        called: Option<RepositoryRequest>,
        now_ms: u64,
        routes: Routes,
        last: Last,
    ) -> bool {
        self.reaped(last, now_ms);
        if std::mem::take(&mut self.activation) {
            self.because(Causes::ACTIVATION);
            self.request(RepositoryRequest::Refresh(RefreshRequest::after(
                None,
                self.observed.version,
            )));
        }
        if let Some(request) = called {
            self.request(request);
        }
        // An input that arrived while the child ran remains work for its successor.
        if self.pending.is_some() || self.identity_pending {
            self.pending = merge_requests(self.parked_request.take(), self.pending);
            self.identity_pending |= std::mem::take(&mut self.parked_identity);
            self.parked = false;
        }
        // A new Refresh supersedes retained retry inputs before acknowledgment
        // can demote it to Repair. A local Repair alone leaves that retry intact.
        if self
            .pending
            .is_some_and(|request| request.work() == RepositoryWork::Refresh)
        {
            self.pending = merge_requests(self.retry_request.take(), self.pending);
        }
        let (pending, acknowledged) = self.demote_fulfilled(self.pending);
        self.pending = pending;
        let (retry, retry_acknowledged) = self.demote_fulfilled(self.retry_request);
        if acknowledged || retry_acknowledged {
            self.acknowledged(now_ms);
        }
        self.retry_request = retry;
        self.refresh_due(now_ms, routes);
        if let Some(RepositoryRequest::Refresh(request)) = self.pending {
            if self.observed.fulfills(request) {
                self.pending = Some(RepositoryRequest::Repair);
                self.acknowledged(now_ms);
            } else if let Some(remaining) = self.observed.retry_for(request)
                && remaining > 0
            {
                self.retry_request = merge_requests(self.retry_request, self.pending.take());
                self.retry.at = Some(now_ms.saturating_add(remaining));
                self.retry_cause = Causes::RETRY_WAVE;
                self.started = Some((now_ms, RECHECK_MS));
            }
        }
        if !self.parked
            && let Some(RepositoryRequest::Refresh(request)) = self.retry_request
            && let Some(remaining) = self.observed.retry_for(request)
        {
            self.retry.at = Some(now_ms.saturating_add(remaining));
            self.retry_cause = Causes::RETRY_WAVE;
        }
        if last == Last::Running {
            return false;
        }
        // A future repository retry cannot defer local identity debt, nor can its clock
        // spend identity's independent failure backoff. No second child or timer is started.
        if self.identity_pending || self.identity_retry.due(now_ms) {
            if self.identity_retry.due(now_ms) {
                self.because(Causes::RETRY_IDENTITY);
            }
            self.pending = merge_requests(self.pending, Some(RepositoryRequest::Repair));
        }
        if self.pending.is_none() && !self.retry.due(now_ms) {
            return false;
        }
        if self.retry.due(now_ms) {
            self.because(self.retry_cause);
        }
        self.noted = StartNote {
            causes: std::mem::take(&mut self.causes),
            moved: std::mem::take(&mut self.moved),
            moves: std::mem::take(&mut self.moves),
        };
        // Any admitted full pass can carry still-owed identity alongside its other work.
        // Keep that subject's custody through the actual child outcome (including start None).
        self.identity_running = self.identity_pending || self.identity_retry.at.is_some();
        if self.identity_running {
            self.identity_pending = false;
            self.identity_retry.started();
        }
        let pending = self.pending.take();
        // A checkout wake can be repaired now without repeating a failed remote refresh.
        // Only its own deadline, or a new Refresh request, consumes that stronger debt.
        let consume_retry = self.retry.due(now_ms)
            || pending.is_some_and(|request| request.work() == RepositoryWork::Refresh)
            || self
                .retry_request
                .is_none_or(|request| request.work() != RepositoryWork::Refresh);
        let retry_request = if consume_retry {
            self.retry_request.take()
        } else {
            None
        };
        self.repository_request = merge_requests(retry_request, pending).unwrap_or_default();
        if consume_retry {
            self.retry.started();
        }
        if self.repository_work() == RepositoryWork::Refresh {
            self.started = Some((now_ms, RECHECK_MS));
        }
        true
    }

    fn demote_fulfilled(
        &self,
        request: Option<RepositoryRequest>,
    ) -> (Option<RepositoryRequest>, bool) {
        match request {
            Some(RepositoryRequest::Refresh(request)) if self.observed.fulfills(request) => {
                // A repository receipt covers only its row. Every trigger still
                // owes its owned full pass, including failures in other rows.
                (Some(RepositoryRequest::Repair), true)
            }
            _ => (request, false),
        }
    }

    fn acknowledged(&mut self, now_ms: u64) {
        if let Some(remaining) = self.observed.recheck_ms {
            self.started = Some((now_ms, remaining));
        }
    }

    fn refresh_due(&mut self, now_ms: u64, routes: Routes) {
        let moved = self
            .routes
            .replace(routes)
            .is_some_and(|before| before != routes);
        if moved {
            self.parked = false;
        } else if self.parked {
            return;
        }
        let went_back = self.started.is_some_and(|(at, _)| now_ms < at);
        let request = if moved || went_back {
            if moved {
                self.because(Causes::ROUTES);
            }
            if went_back {
                self.because(Causes::CLOCK);
                self.started = Some((now_ms, RECHECK_MS));
            }
            Some(RefreshRequest::after(None, self.observed.version))
        } else if self.started.is_none_or(|(at, wait)| now_ms - at >= wait) {
            self.because(Causes::RECHECK);
            Some(RefreshRequest::ensure(None, self.observed.version))
        } else {
            None
        };
        if let Some(request) = request {
            self.request(RepositoryRequest::Refresh(request));
        }
    }
}

fn merge_requests(
    older: Option<RepositoryRequest>,
    newer: Option<RepositoryRequest>,
) -> Option<RepositoryRequest> {
    match (older, newer) {
        (Some(older), Some(newer)) => Some(older.merge(newer)),
        _ => newer.or(older),
    }
}

#[cfg(test)]
#[path = "converge_tests.rs"]
mod tests;
