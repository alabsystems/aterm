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

use crate::client::SyncOutcome;
use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::net::IpAddr;
use std::path::Path;

mod refresh;
pub use refresh::{
    RefreshObservation, RefreshRequest, RefreshScope, RefreshVersion, RepositoryRequest,
};

/// The exit status a runner ends with while another pass holds the pass's lock: it did not run.
pub const HELD: u8 = 75;

/// The runner ran without failure but admitted work still waits on a precondition or readback.
pub const UNFINISHED: u8 = 76;

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
/// subscriber is a `Background` launchd job, where every thread runs at priority 4 whatever
/// class it asks for, and the pass writes the fleet book under its lock, whose taker is lifted
/// to the default class while it holds it, a lift that band defeats. That runner holds its own
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
    /// It exited unsuccessfully, or ended by a signal.
    Failed,
    /// It ended without running, another pass holding the lock ([`HELD`]).
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
            Ok(Some(status)) if status.code() == Some(i32::from(HELD)) => Self::Held,
            Ok(Some(status)) if status.code() == Some(i32::from(UNFINISHED)) => Self::Unfinished,
            Ok(Some(status)) if status.success() => Self::Ended,
            Ok(Some(_)) => Self::Failed,
        };
        if last != Self::Running {
            runner.take();
        }
        last
    }
}

/// Unfinished and failed runner pacing shared by the subscriber, mesh node and window lane.
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
            Last::Ended => {
                self.at = None;
                self.delay = 0;
            }
            // A held lock owes a pass, without spending or resetting its unfinished backoff.
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
/// version, after the first healthy pass that follows failed ones (the vault came back, or this
/// Mac came back to it: `docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2), and after any other
/// healthy pass once this Mac's routes moved or [`RECHECK_MS`] passed (or the clock went back)
/// since the last start. One runner at a time: a start that comes due while the last one runs,
/// or after one that found the lock held, is owed to the next round, so a version applied during
/// a pass is never left to the next recheck. An unfinished or failed runner retries on a
/// bounded backoff; every time here is elapsed milliseconds on the subscriber's monotonic clock.
#[derive(Debug, Default)]
pub struct Due {
    /// The last refresh boundary's local start and remaining wait. A shared
    /// acknowledgment preserves its actual remaining time; repairs never postpone it.
    started: Option<(u64, u64)>,
    /// Routes already observed, including a request queued while the lane was held.
    routes: Option<Routes>,
    /// A start came due while the last runner ran, or the last one found the lock held.
    pending: Option<RepositoryRequest>,
    /// The last chosen run, retained until its terminal result and used by the starter.
    repository_request: RepositoryRequest,
    /// Failed work waits for RunnerRetry, unless new work starts a stronger combined run.
    retry_request: Option<RepositoryRequest>,
    /// One shared receipt sample, read before the corresponding local-input observation.
    observed: RefreshObservation,
    /// A pass failed since the last healthy one ([`Due::failed`]).
    failing: bool,
    /// Automatic work was held. Its next admitted input capture owes fresh readback.
    activation: bool,
    retry: RunnerRetry,
}

impl Due {
    /// Retain readback while automatic work is explicitly held, without reading inputs.
    pub(crate) fn paused(&mut self) {
        self.activation = true;
    }

    pub(crate) fn activation_pending(&self) -> bool {
        self.activation
    }

    /// Supply the same receipt facts used to bind this round's input transition.
    pub fn observe(&mut self, observed: RefreshObservation) {
        self.observed = observed;
    }

    /// Retain a request while the owned runner or a shared retry still holds the lane.
    pub(crate) fn request(&mut self, request: RepositoryRequest) {
        self.pending = merge_requests(self.pending, Some(request));
    }

    /// Service the same job while another subscriber lane is waiting on HTTP.
    pub(crate) fn poll(&mut self, now_ms: u64, routes: Routes, last: Last) -> bool {
        self.start(None, now_ms, routes, last)
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
        let applied = matches!(outcome, SyncOutcome::Applied { .. });
        let back = std::mem::take(&mut self.failing);
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
        self.start(
            matches!(outcome, SyncOutcome::Applied { .. }).then_some(RepositoryRequest::Repair),
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
    /// completion ends its failure streak; a held lock stays owed without consuming backoff.
    /// A Repair that ran beside deferred Refresh debt cannot retire or reschedule that debt.
    pub fn reaped(&mut self, last: Last, now_ms: u64) {
        if self.repository_work() == RepositoryWork::Repair
            && self
                .retry_request
                .is_some_and(|request| request.work() == RepositoryWork::Refresh)
        {
            // The stronger retained refresh includes acquisition/repair. A held
            // repair cannot create an immediate queue or spend that retry budget.
            return;
        }
        // A started job already took its captured work from the queues. A later
        // success cannot clear retry work that arrived while that job ran.
        if last != Last::Ended || self.retry_request.is_none() {
            self.retry.reaped(last, now_ms);
        }
        match last {
            Last::Failed | Last::Unfinished => {
                self.retry_request =
                    merge_requests(Some(self.repository_request), self.retry_request);
            }
            Last::Held => {
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
            Last::None | Last::Running => {}
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
            self.request(RepositoryRequest::Refresh(RefreshRequest::after(
                None,
                self.observed.version,
            )));
        }
        self.pending = merge_requests(self.pending, called);
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
                self.started = Some((now_ms, RECHECK_MS));
            }
        }
        if let Some(RepositoryRequest::Refresh(request)) = self.retry_request
            && let Some(remaining) = self.observed.retry_for(request)
        {
            self.retry.at = Some(now_ms.saturating_add(remaining));
        }
        if self.pending.is_none() && !self.retry.due(now_ms) {
            return false;
        }
        if last == Last::Running {
            return false;
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
        let went_back = self.started.is_some_and(|(at, _)| now_ms < at);
        let request = if moved || went_back {
            if went_back {
                self.started = Some((now_ms, RECHECK_MS));
            }
            Some(RefreshRequest::after(None, self.observed.version))
        } else if self.started.is_none_or(|(at, wait)| now_ms - at >= wait) {
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
mod tests {
    use super::*;
    use crate::materialize::Report;
    use std::net::Ipv4Addr;

    const HOME: Routes = [Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))), None, None];
    const AWAY: Routes = [Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))), None, None];
    const MINUTE: u64 = 60 * 1000;

    fn applied() -> SyncOutcome {
        SyncOutcome::Applied {
            seq: 3,
            report: Report::default(),
        }
    }

    fn unchanged() -> SyncOutcome {
        SyncOutcome::Unchanged { seq: 3 }
    }

    /// One start as the subscriber starts, one more for each version it applies, none for a pass
    /// that changed nothing on the same routes inside the recheck, one when the routes move and
    /// one once the recheck passes (or the clock goes back).
    /// NEGATIVE CONTROL: an `after_pass` that reads an applied version as a plain round starts
    /// nothing for it.
    #[test]
    fn the_subscriber_starts_a_runner_at_start_per_applied_version_and_when_due() {
        let mut due = Due::default();
        let starts = std::cell::Cell::new(0);
        let at = |started: bool| starts.set(starts.get() + usize::from(started));
        at(due.at_start(0, HOME, Last::None));
        assert_eq!(starts.get(), 1, "one at start");
        at(due.after_pass(&applied(), MINUTE, HOME, Last::Ended));
        assert_eq!(starts.get(), 2, "+1 for an applied version");
        at(due.after_pass(&applied(), 2 * MINUTE, HOME, Last::Ended));
        assert_eq!(starts.get(), 3, "+1 for each");
        for minute in 3..30 {
            at(due.after_pass(&unchanged(), minute * MINUTE, HOME, Last::Ended));
        }
        assert_eq!(starts.get(), 3, "0 for passes that changed nothing");
        at(due.after_pass(&unchanged(), 30 * MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 4, "+1 when the routes move");
        at(due.after_pass(&unchanged(), 31 * MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 4, "not again on the routes it started on");
        at(due.after_pass(&unchanged(), 60 * MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 5, "+1 once the recheck passed");
        at(due.after_pass(&unchanged(), MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 6, "+1 for a clock that went back");
    }

    /// A start that comes due while the last runner runs waits for it to end rather than start a
    /// second that would find the lock held; one that found the lock held keeps its request
    /// until the existing five-second retry boundary.
    #[test]
    fn a_start_due_while_a_runner_runs_or_after_a_held_lock_is_owed_to_the_next_round() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        assert!(
            !due.after_pass(&applied(), MINUTE, HOME, Last::Running),
            "never two at once"
        );
        assert!(
            !due.after_pass(&unchanged(), 2 * MINUTE, HOME, Last::Running),
            "still running"
        );
        assert!(
            due.after_pass(&unchanged(), 3 * MINUTE, HOME, Last::Ended),
            "the owed start, once it ended"
        );
        assert!(!due.after_pass(&unchanged(), 4 * MINUTE, HOME, Last::Ended));
        assert!(
            !due.after_pass(&unchanged(), 5 * MINUTE, HOME, Last::Held),
            "another pass held the lock: retain the request without a per-look loop"
        );
        assert!(!due.after_pass(
            &unchanged(),
            5 * MINUTE + FIRST_RETRY_MS - 1,
            HOME,
            Last::None
        ));
        assert!(due.after_pass(&unchanged(), 5 * MINUTE + FIRST_RETRY_MS, HOME, Last::None));
        assert!(!due.after_pass(&unchanged(), 6 * MINUTE, HOME, Last::Ended));
    }

    /// The first healthy pass after failed ones starts a runner, on the routes and inside the
    /// recheck of the last start, and the one after it does not.
    /// NEGATIVE CONTROL: an `after_pass` that forgets the failed passes starts nothing.
    #[test]
    fn a_healthy_pass_after_failed_ones_is_due_once() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        due.failed();
        due.failed();
        assert!(
            due.after_pass(&unchanged(), MINUTE, HOME, Last::Ended),
            "back after errors"
        );
        assert!(!due.after_pass(&unchanged(), 2 * MINUTE, HOME, Last::Ended));
        due.failed();
        assert!(
            !due.after_pass(&unchanged(), 3 * MINUTE, HOME, Last::Running),
            "never two at once"
        );
        assert!(
            due.after_pass(&unchanged(), 4 * MINUTE, HOME, Last::Ended),
            "owed once the runner ended"
        );
    }

    #[test]
    fn an_unfinished_runner_retries_unchanged_inputs_and_success_ends_the_retry() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        // The subscriber reaps before its source round, even if that round is unhealthy.
        due.reaped(Last::Unfinished, 1_000);
        for at in [1_000, 1_001, 5_999] {
            assert!(!due.after_pass(&unchanged(), at, HOME, Last::None));
        }
        assert!(
            due.after_pass(&unchanged(), 6_000, HOME, Last::None),
            "unfinished work gets another pass without a new route, token or inventory"
        );
        assert!(
            !due.after_pass(&unchanged(), 6_001, HOME, Last::Running),
            "the retry remains the only runner"
        );
        due.reaped(Last::Ended, 6_002);
        for at in [6_003, 11_000, RECHECK_MS] {
            assert!(
                !due.after_pass(&unchanged(), at, HOME, Last::None),
                "success leaves only the ordinary recheck due"
            );
        }
        assert!(due.after_pass(&unchanged(), 6_000 + RECHECK_MS, HOME, Last::None));
        // Success reset the failure streak: another failure begins at five seconds again.
        let failed_at = 6_001 + RECHECK_MS;
        due.reaped(Last::Failed, failed_at);
        assert!(!due.after_pass(&unchanged(), failed_at + 4_999, HOME, Last::None));
        assert!(due.after_pass(&unchanged(), failed_at + 5_000, HOME, Last::None));
    }

    #[test]
    fn consecutive_runner_failures_back_off_without_a_hot_loop_and_stay_bounded() {
        let mut due = Due::default();
        let mut at = 0;
        assert!(due.at_start(at, HOME, Last::None));
        for seconds in [5, 10, 20, 40, 80, 160, 300, 300] {
            at += 1;
            due.reaped(Last::Failed, at);
            let ready = at + seconds * 1000;
            assert!(!due.after_pass(&unchanged(), at, HOME, Last::None));
            assert!(!due.after_pass(&unchanged(), ready - 1, HOME, Last::None));
            assert!(due.after_pass(&unchanged(), ready, HOME, Last::None));
            at = ready;
        }
    }

    #[test]
    fn held_and_running_runners_preserve_failed_retry_and_coalesced_work() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        due.reaped(Last::Failed, 1);
        assert!(due.after_pass(&unchanged(), 5_001, HOME, Last::None));
        due.reaped(Last::Held, 5_002);
        assert!(
            !due.after_pass(&unchanged(), 5_002, HOME, Last::None),
            "a held lock keeps its existing bounded retry"
        );
        assert!(!due.after_pass(&unchanged(), 10_001, HOME, Last::None));
        assert!(due.after_pass(&unchanged(), 10_002, HOME, Last::None));
        due.reaped(Last::Failed, 10_003);
        assert!(!due.after_pass(&unchanged(), 20_002, HOME, Last::None));
        assert!(
            due.after_pass(&unchanged(), 20_003, HOME, Last::None),
            "a lock hold did not erase the failure streak"
        );
        assert!(!due.after_pass(&applied(), 20_004, HOME, Last::Running));
        assert!(!due.after_pass(&unchanged(), 20_005, HOME, Last::Running));
        assert!(due.after_pass(&unchanged(), 20_006, HOME, Last::Ended));
        assert!(!due.after_pass(&unchanged(), 20_007, HOME, Last::Ended));
    }

    #[test]
    fn local_repairs_and_unrelated_versions_do_not_postpone_the_remote_recheck() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        assert_eq!(due.repository_work(), RepositoryWork::Refresh);
        assert!(due.after_pass(&applied(), 1, HOME, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        due.failed();
        assert!(due.after_pass(&unchanged(), 2, HOME, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(due.after_local_progress(
            RepositoryWork::Repair,
            RECHECK_MS - 1,
            HOME,
            Last::Ended
        ));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(due.poll(RECHECK_MS, HOME, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    }

    #[test]
    fn pending_and_held_work_keeps_the_strongest_refresh_request() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        assert!(due.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(!due.after_local_progress(RepositoryWork::Refresh, 2, HOME, Last::Running));
        assert!(!due.after_local_progress(RepositoryWork::Repair, 3, HOME, Last::Running));
        assert!(due.poll(4, HOME, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Refresh);
        assert!(!due.poll(5, HOME, Last::Ended));
        due.reaped(Last::Held, 6);
        assert!(due.after_local_progress(RepositoryWork::Repair, 7, HOME, Last::None));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(!due.poll(5_005, HOME, Last::Ended));
        assert!(due.poll(5_006, HOME, Last::None));
        assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    }

    #[test]
    fn checkout_repairs_preserve_a_failed_refresh_deadline_and_backoff() {
        for refresh_end in [Last::Failed, Last::Unfinished] {
            for repair_end in [Last::Ended, Last::Failed, Last::Unfinished] {
                let mut due = Due::default();
                assert!(due.at_start(0, HOME, Last::None));
                due.reaped(refresh_end, 1_000);
                assert!(due.after_local_progress(RepositoryWork::Repair, 1_001, HOME, Last::None));
                assert_eq!(due.repository_work(), RepositoryWork::Repair);
                due.reaped(repair_end, 1_002);
                assert!(!due.poll(5_999, HOME, Last::None));
                assert!(due.poll(6_000, HOME, Last::None));
                assert_eq!(due.repository_work(), RepositoryWork::Refresh);
                due.reaped(Last::Failed, 6_001);
                assert!(!due.poll(16_000, HOME, Last::None));
                assert!(due.poll(16_001, HOME, Last::None));
                assert_eq!(due.repository_work(), RepositoryWork::Refresh);
            }
        }
    }

    #[test]
    fn a_new_source_refresh_starts_before_retained_refresh_retry_is_due() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        due.reaped(Last::Failed, 1_000);
        assert!(due.after_local_progress(RepositoryWork::Repair, 1_001, HOME, Last::None));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(!due.after_local_progress(RepositoryWork::Refresh, 1_002, HOME, Last::Running));
        assert!(!due.after_local_progress(RepositoryWork::Repair, 1_003, HOME, Last::Running));
        assert!(due.poll(1_004, HOME, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Refresh);
        assert!(!due.poll(6_000, HOME, Last::Ended));
    }

    #[test]
    fn a_failed_repair_retries_locally_until_routes_request_a_refresh() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        assert!(due.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::Ended));
        due.reaped(Last::Unfinished, 2);
        assert!(!due.poll(1 + FIRST_RETRY_MS, HOME, Last::None));
        assert!(due.poll(2 + FIRST_RETRY_MS, HOME, Last::None));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(!due.poll(3 + FIRST_RETRY_MS, AWAY, Last::Running));
        assert!(due.poll(4 + FIRST_RETRY_MS, AWAY, Last::Ended));
        assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    }

    #[test]
    fn producers_share_the_remote_wave_and_keep_their_generic_passes() {
        let scope = RefreshScope::new([1; 32]);
        let before = RefreshObservation {
            version: RefreshVersion {
                input: 1,
                attempt: 1,
                fulfilled: 1,
            },
            scope: Some(RefreshScope::new([0; 32])),
            recheck_ms: Some(RECHECK_MS),
            ..RefreshObservation::default()
        };
        let request = RepositoryRequest::Refresh(RefreshRequest::inputs(scope, before.version));
        let mut node = Due::default();
        let mut subscriber = Due::default();
        for due in [&mut node, &mut subscriber] {
            due.observe(before);
            assert!(
                due.at_start(0, HOME, Last::None),
                "startup without a current scope cannot inherit the old source's receipt"
            );
            assert!(
                due.after_request(
                    RepositoryRequest::Refresh(RefreshRequest::ensure(
                        before.scope,
                        before.version
                    )),
                    1,
                    HOME,
                    Last::Ended,
                ),
                "a known matching ensure still owes its own generic pass"
            );
            assert_eq!(due.repository_work(), RepositoryWork::Repair);
            assert!(due.after_request(request, 2, HOME, Last::Ended));
            assert_eq!(due.repository_request(), request);
        }
        subscriber.reaped(Last::Held, 3);
        let finished = RefreshObservation {
            version: RefreshVersion {
                input: 2,
                attempt: 2,
                fulfilled: 2,
            },
            scope: Some(scope),
            recheck_ms: Some(RECHECK_MS - 100),
            ..before
        };
        subscriber.observe(finished);
        assert!(
            !subscriber.poll(100, HOME, Last::None),
            "the held follower keeps its bounded retry after sharing the remote wave"
        );
        assert!(
            !subscriber.poll(FIRST_RETRY_MS + 2, HOME, Last::None),
            "repository acknowledgment cannot accelerate the generic retry"
        );
        assert!(subscriber.poll(FIRST_RETRY_MS + 3, HOME, Last::None));
        assert_eq!(subscriber.repository_work(), RepositoryWork::Repair);
        assert!(!subscriber.poll(FIRST_RETRY_MS + 4, HOME, Last::Ended));
        subscriber.observe(RefreshObservation {
            recheck_ms: Some(0),
            ..finished
        });
        assert!(
            subscriber.poll(RECHECK_MS, HOME, Last::None),
            "a delayed acknowledgment preserves the shared deadline"
        );

        let mut startup = Due::default();
        startup.observe(finished);
        startup.request(RepositoryRequest::Refresh(RefreshRequest::ensure(
            finished.scope,
            finished.version,
        )));
        assert!(startup.at_start(0, HOME, Last::None));
        assert_eq!(startup.repository_work(), RepositoryWork::Repair);
        assert!(!startup.poll(1, HOME, Last::Ended));
        assert!(!startup.poll(2, HOME, Last::None));

        let mut unknown = Due::default();
        unknown.observe(RefreshObservation {
            recheck_ms: Some(0),
            ..finished
        });
        assert!(unknown.at_start(10, HOME, Last::None));
        for (at, last) in [
            (11, Last::Ended),
            (12, Last::None),
            (RECHECK_MS + 9, Last::None),
        ] {
            assert!(
                !unknown.poll(at, HOME, last),
                "an expired old receipt and successful NotApplicable cannot create a per-look loop"
            );
        }
        assert!(unknown.poll(RECHECK_MS + 10, HOME, Last::None));
        assert!(
            unknown.after_request(
                RepositoryRequest::Refresh(RefreshRequest::inputs(scope, finished.version)),
                RECHECK_MS + 11,
                HOME,
                Last::Ended,
            ),
            "a real source transition interrupts ordinary unknown-source pacing"
        );
    }

    #[test]
    fn shared_refresh_retry_allows_local_repair_without_losing_its_original_request() {
        let refresh = RefreshRequest::inputs(RefreshScope::new([1; 32]), RefreshVersion::default());
        let request = RepositoryRequest::Refresh(refresh);
        let mut due = Due::default();
        let failed = RefreshObservation {
            pending: Some(refresh),
            retry_ms: Some(5_000),
            ..RefreshObservation::default()
        };
        due.observe(failed);
        assert!(!due.after_request(request, 0, HOME, Last::None));
        due.observe(RefreshObservation {
            retry_ms: Some(4_999),
            ..failed
        });
        assert!(due.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::None));
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        due.observe(RefreshObservation {
            retry_ms: Some(4_998),
            ..failed
        });
        assert!(
            !due.poll(2, HOME, Last::Held),
            "a held repair is subsumed without spinning or moving the refresh deadline"
        );
        due.observe(RefreshObservation {
            retry_ms: Some(0),
            ..failed
        });
        assert!(due.poll(5_000, HOME, Last::None));
        assert_eq!(due.repository_request(), request);

        let fulfilled = RefreshObservation {
            scope: refresh.scope,
            version: RefreshVersion {
                input: 1,
                attempt: 1,
                fulfilled: 1,
            },
            recheck_ms: Some(RECHECK_MS),
            ..RefreshObservation::default()
        };
        // This owner's full job can fail after its Native repository row succeeds.
        // The repository receipt does not acknowledge the remaining generic rows.
        for outcome in [Last::Failed, Last::Unfinished] {
            let mut owned = Due::default();
            assert!(owned.after_request(request, 0, HOME, Last::None));
            owned.observe(fulfilled);
            assert!(!owned.poll(1, HOME, outcome));
            assert!(!owned.poll(FIRST_RETRY_MS, HOME, Last::None));
            assert!(owned.poll(FIRST_RETRY_MS + 1, HOME, Last::None));
            assert_eq!(owned.repository_work(), RepositoryWork::Repair);
            assert!(!owned.poll(FIRST_RETRY_MS + 2, HOME, Last::Ended));

            // The successful owned generic job clears its captured debt. A later
            // held-only trigger still owes its own generic pass, at the held deadline.
            let forced = RepositoryRequest::Refresh(RefreshRequest::after(
                fulfilled.scope,
                fulfilled.version,
            ));
            assert!(owned.after_request(forced, FIRST_RETRY_MS + 1_000, HOME, Last::None));
            owned.observe(RefreshObservation {
                version: RefreshVersion {
                    attempt: 2,
                    fulfilled: 2,
                    ..fulfilled.version
                },
                ..fulfilled
            });
            assert!(!owned.poll(FIRST_RETRY_MS + 1_001, HOME, Last::Held));
            assert!(!owned.poll(2 * FIRST_RETRY_MS + 1_000, HOME, Last::None));
            assert!(owned.poll(2 * FIRST_RETRY_MS + 1_001, HOME, Last::None));
            assert_eq!(owned.repository_work(), RepositoryWork::Repair);
            assert!(!owned.poll(2 * FIRST_RETRY_MS + 1_002, HOME, Last::Ended));
        }

        // Repair can run beside another owner's deferred Refresh without spending
        // that Refresh's failure streak. Its own non-repository failure still survives.
        for outcome in [Last::Held, Last::Ended, Last::Failed, Last::Unfinished] {
            let mut local = Due::default();
            local.observe(failed);
            assert!(!local.after_request(request, 0, HOME, Last::None));
            local.observe(RefreshObservation {
                retry_ms: Some(FIRST_RETRY_MS - 1),
                ..failed
            });
            assert!(local.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::None));
            local.observe(fulfilled);
            assert!(!local.poll(2, HOME, outcome));
            assert!(!local.poll(FIRST_RETRY_MS - 1, HOME, Last::None));
            assert!(local.poll(FIRST_RETRY_MS, HOME, Last::None));
            assert_eq!(local.repository_work(), RepositoryWork::Repair);
            assert!(!local.poll(FIRST_RETRY_MS + 1, HOME, Last::Ended));
        }

        let mut during = Due::default();
        assert!(during.after_request(request, 0, HOME, Last::None));
        assert!(!during.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::Running));
        during.observe(fulfilled);
        assert!(!during.after_request(
            RepositoryRequest::Refresh(RefreshRequest::ensure(fulfilled.scope, fulfilled.version)),
            2,
            HOME,
            Last::Running,
        ));
        assert!(during.poll(3, HOME, Last::Ended));
        assert_eq!(during.repository_work(), RepositoryWork::Repair);
        assert!(!during.poll(4, HOME, Last::Ended));
        assert!(!during.poll(5, HOME, Last::None));

        // A completed new source consumes the old source's retained retry before
        // its Refresh becomes Repair, so the obsolete source cannot revive later.
        let mut superseded = Due::default();
        superseded.observe(failed);
        assert!(!superseded.after_request(request, 0, HOME, Last::None));
        let winner = RefreshObservation {
            scope: Some(RefreshScope::new([2; 32])),
            version: RefreshVersion {
                input: 2,
                attempt: 2,
                fulfilled: 2,
            },
            recheck_ms: Some(RECHECK_MS),
            ..RefreshObservation::default()
        };
        superseded.observe(winner);
        assert!(superseded.after_request(
            RepositoryRequest::Refresh(RefreshRequest::inputs(
                winner.scope.unwrap(),
                fulfilled.version,
            )),
            1,
            HOME,
            Last::None,
        ));
        assert_eq!(superseded.repository_work(), RepositoryWork::Repair);
        assert!(!superseded.poll(2, HOME, Last::Ended));
        assert!(!superseded.poll(FIRST_RETRY_MS, HOME, Last::None));
    }

    #[test]
    fn a_route_event_captures_one_boundary_while_the_lan_is_held() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        let observed = RefreshObservation {
            version: RefreshVersion {
                attempt: 2,
                ..RefreshVersion::default()
            },
            ..RefreshObservation::default()
        };
        due.observe(observed);
        due.hold(1, AWAY, Last::Running);
        due.observe(RefreshObservation {
            version: RefreshVersion {
                attempt: 3,
                ..observed.version
            },
            ..observed
        });
        due.hold(2, AWAY, Last::Running);
        assert!(due.poll(3, AWAY, Last::Ended));
        let RepositoryRequest::Refresh(request) = due.repository_request() else {
            panic!("a route move requests refresh");
        };
        assert_eq!(
            request.after_attempt,
            Some(2),
            "waiting must not move the event's boundary"
        );
    }

    /// The runner keeps its starting app's privacy responsibility and leaves only the
    /// subscriber's background band. A started child below proves its own session.
    #[test]
    fn the_runner_is_init_converge_the_subscribers_out_of_the_band_never_disclaiming() {
        let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        for trigger in [SUBSCRIBER, WINDOW, SETUP] {
            for work in [RepositoryWork::Repair, RepositoryWork::Refresh] {
                let command = runner(exe, trigger, work);
                assert_eq!(command.get_program(), exe);
                let args: Vec<_> = command.get_args().collect();
                assert_eq!(
                    args,
                    [
                        "init",
                        "--converge",
                        "--trigger",
                        trigger,
                        "--repository-work",
                        work.word()
                    ]
                );
                assert!(
                    !command.get_disclaim_responsibility(),
                    "the runner's privacy-gated acts stay the starting app's"
                );
                assert_eq!(
                    command.leaves_background_band(),
                    trigger == SUBSCRIBER,
                    "only the subscriber's runner leaves its starter's background band ({trigger})"
                );
            }
            let request = RepositoryRequest::Refresh(RefreshRequest::after(
                Some(RefreshScope::new([7; 32])),
                RefreshVersion {
                    input: 2,
                    attempt: 4,
                    fulfilled: 3,
                },
            ));
            let command = runner_requested(exe, trigger, request);
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[4], "--repository-request");
            assert_eq!(
                aterm_json::from_str::<RepositoryRequest>(args[5].to_str().unwrap()).unwrap(),
                request,
                "the original request boundary survives runner IPC"
            );
            assert!(!command.get_disclaim_responsibility());
            assert_eq!(command.leaves_background_band(), trigger == SUBSCRIBER);
        }
    }

    /// A runner that does not end by itself leaves the start, and the reading of how it stands,
    /// returning at once: the subscriber's long-poll never waits on a pass. The child leads a
    /// process group of its own (the session `setsid` made), and one that ends with [`HELD`]
    /// reads as held.
    #[cfg(unix)]
    #[test]
    fn starting_a_runner_never_waits_for_it_and_gives_it_its_own_session() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let script = |name: &str, body: String| {
            let path = scratch.path().join(name);
            std::fs::write(&path, format!("#!{}\n{body}\n", sh.display())).expect("script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            path
        };
        let endless = script("endless", format!("exec {} 600", sleep.display()));
        let mut runner =
            Some(start(&endless, "subscriber", RepositoryWork::Refresh).expect("start a runner"));
        assert_eq!(Last::of(&mut runner), Last::Running);
        let child = runner.as_mut().unwrap();
        let pid = i32::try_from(child.id()).expect("a pid");
        let lineage = aterm_uds::process::read_lineage(pid).expect("its lineage");
        assert_eq!(
            lineage.pgid,
            child.id(),
            "it leads a group (and the session setsid made) of its own"
        );
        child.kill().expect("kill the stand-in");
        child.wait().expect("reap it");
        assert_eq!(Last::of(&mut runner), Last::Failed);
        assert!(runner.is_none());
        assert_eq!(
            Last::of(&mut runner),
            Last::None,
            "a signal is consumed once"
        );
        for (code, expected) in [
            (0, Last::Ended),
            (1, Last::Failed),
            (HELD, Last::Held),
            (UNFINISHED, Last::Unfinished),
        ] {
            let exe = script(&format!("exit-{code}"), format!("exit {code}"));
            let mut runner =
                Some(start(&exe, "subscriber", RepositoryWork::Refresh).expect("start a runner"));
            runner.as_mut().unwrap().wait().expect("reap it");
            assert_eq!(Last::of(&mut runner), expected);
            assert_eq!(
                Last::of(&mut runner),
                Last::None,
                "exit {code} is consumed once"
            );
        }
    }
}
