// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending subscriber can reach a member when its LAN changes, its broker arrives or a
//! native member responder becomes ready,
//! even while its vault request is held in a long poll. This worker invokes the authenticated
//! member callback; it provides no discovery, proof or admission authority of its own.

use super::{
    MemberArrivals, MembersAsked, Paths, ask_members_if_due, ask_members_pause, swarm_lets,
};
use crate::converge::{Last, RunnerRetry};
use aterm_uds::ready::Doorbell;
use aterm_uds::stop::Stop;
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ScopedJoinHandle;
use std::time::{Duration, Instant};

/// The least time between two looks (the projection's maintenance,
/// [`ProjectionEvent::Maintenance`], and its observation, [`ProjectionEvent::Observe`]) that a
/// ring of its inputs, its runner's exit or its own deadline owes ([`Projected`]). A change of
/// network or an arrival looks at once.
const LOOK: Duration = Duration::from_secs(1);
const REST: Duration = Duration::from_secs(1);

pub(super) enum ProjectionEvent {
    Maintenance,
    Observe,
    /// Listener observations may have been interrupted; request ordinary readback,
    /// without claiming admission or materialization.
    Recovered,
    Ended,
    Admitted,
    BrokerReady,
}

/// What the worker waits on: whether a change of network came by `deadline` (`None`: with no
/// deadline). A wait may end early with none (`also` ready); the worker reads those sources
/// separately.
pub(super) trait Changes: Send {
    /// Check retained observation custody before another readiness source is consumed.
    fn check(&self) -> std::io::Result<()> {
        Ok(())
    }
    fn wait(&mut self, deadline: Option<Instant>, also: &[BorrowedFd<'_>]) -> bool;
    fn reset(&mut self) {}
}

#[cfg(test)]
impl<F: FnMut(Option<Instant>) -> bool + Send> Changes for F {
    fn wait(&mut self, deadline: Option<Instant>, _also: &[BorrowedFd<'_>]) -> bool {
        self(deadline)
    }
}

/// What the worker serves: the projection's events, and what owes its next look besides a change
/// of network, an arrival and a ring.
pub(super) trait Projected: Sync {
    fn event(&self, event: ProjectionEvent);
    /// When the projection next owes a look with nothing heard (its maintenance, its runner's
    /// retry or recheck, an input no watch covers), asked after each look; `None`: none.
    fn next_look(&self) -> Option<Instant> {
        None
    }
    /// Descriptors whose readiness owes a look: a runner's exit.
    fn wakes(&self) -> Vec<Arc<OwnedFd>> {
        Vec::new()
    }
}

impl<F: Fn(ProjectionEvent) + Sync> Projected for F {
    fn event(&self, event: ProjectionEvent) {
        self(event);
    }
}

/// How long a wait that can poll nothing rests toward `deadline` (`None`: none): until it, at
/// most a [`LOOK`], so a worker that hears nothing still sees its stop and its deadlines.
pub(super) fn rest(deadline: Option<Instant>) -> Duration {
    deadline.map_or(LOOK, |at| {
        at.saturating_duration_since(Instant::now()).min(LOOK)
    })
}

/// The doorbells others ring for the worker.
#[derive(Clone, Copy, Default)]
pub(super) struct Rings<'a> {
    /// Rung when an input the projection reads changed, or its upkeep ended: a look is owed.
    pub(super) inputs: Option<&'a Doorbell>,
    /// Rung when the HTTP thread's asking began, ended or moved its next ask ([`MembersAsked`]):
    /// the worker reads its asking again, and owes no look.
    pub(super) members: Option<&'a Doorbell>,
    /// Rung when a call moved what the projection's runner owes ([`Projected::wakes`],
    /// [`Projected::next_look`]), another thread's included: the worker waits on them again, and
    /// owes no look.
    pub(super) projection: Option<&'a Doorbell>,
}

pub(super) struct NativeChanges {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    route: std::io::Result<crate::network::RouteChanges>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    open: Box<dyn FnMut() -> std::io::Result<crate::network::RouteChanges> + Send>,
}

impl NativeChanges {
    pub(super) fn new() -> Self {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        return Self::opening(crate::network::RouteChanges::open);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        Self {}
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn opening(
        mut open: impl FnMut() -> std::io::Result<crate::network::RouteChanges> + Send + 'static,
    ) -> Self {
        Self {
            route: open(),
            open: Box::new(open),
        }
    }
}

impl Changes for NativeChanges {
    fn check(&self) -> std::io::Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        self.route
            .as_ref()
            .map(|_| ())
            .map_err(|error| std::io::Error::new(error.kind(), error.to_string()))?;
        Ok(())
    }

    fn reset(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.route = (self.open)();
        }
    }

    /// A change of network ends the wait as one; `also`'s readiness and the deadline end it as
    /// none. Messages that name no change never end it. When the route socket or poll(2) fails,
    /// the rest of the wait is spent on `also` alone, as a failed socket would poll ready again
    /// at once; the next wait hears the socket again, so a failure that passes (a read's error,
    /// a poll(2) that could not allocate) never leaves the worker deaf to later changes. A wait
    /// that can poll nothing rests ([`rest`]) and returns.
    fn wait(&mut self, deadline: Option<Instant>, also: &[BorrowedFd<'_>]) -> bool {
        let immediate = deadline.is_some_and(|at| at <= Instant::now());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let mut route = self.route.as_ref().ok();
        loop {
            let fds = also.to_vec();
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            let fds = {
                let mut fds = fds;
                if let Some(route) = route {
                    use std::os::fd::AsFd as _;
                    fds.push(route.as_fd());
                }
                fds
            };
            if fds.is_empty() {
                std::thread::sleep(rest(deadline));
                return false;
            }
            let ready = if immediate {
                let interests: Vec<_> = fds
                    .iter()
                    .map(|fd| aterm_uds::ready::Watch::new(*fd, aterm_uds::ready::Interest::Read))
                    .collect();
                aterm_uds::ready::any_interests_now(&interests)
            } else {
                aterm_uds::ready::any(&fds, deadline)
            };
            let Ok(ready) = ready else {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if route.take().is_some() {
                    // Retry only the retained additional descriptors, at the same
                    // deadline. A second poll failure is rested out, never spun.
                    continue;
                }
                std::thread::sleep(rest(deadline));
                return false;
            };
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if ready.contains(&also.len()) {
                match route.and_then(crate::network::RouteChanges::read_waiting) {
                    Some(true) => return true,
                    Some(false) => {}
                    None => route = None,
                }
            }
            if immediate || ready.is_empty() || ready.iter().any(|index| *index < also.len()) {
                return false;
            }
        }
    }
}

#[derive(Default)]
struct NativeArrivals {
    source: Option<Box<dyn MemberArrivals>>,
    retry: RunnerRetry,
    error: Option<String>,
}

impl NativeArrivals {
    fn failed(&mut self, error: std::io::Error, now: u64, log: &dyn Fn(&str)) {
        self.source = None;
        self.retry.reaped(Last::Failed, now);
        let why = error.to_string();
        if self.error.as_ref() != Some(&why) {
            log(&format!("native member readiness waits: {why}"));
            self.error = Some(why);
        }
    }

    /// Return whether listening custody was newly established. Source failures
    /// retain their own ordinary retry without stopping route/projection work.
    fn maintain(
        &mut self,
        paths: &Paths,
        open: &dyn Fn(&Paths) -> std::io::Result<Box<dyn MemberArrivals>>,
        refresh: bool,
        now: u64,
        log: &dyn Fn(&str),
    ) -> bool {
        if let Some(source) = &mut self.source {
            if refresh && let Err(error) = source.refresh() {
                self.failed(error, now, log);
            }
            return false;
        }
        if refresh {
            self.retry = RunnerRetry::default();
        }
        if self
            .retry
            .remaining(now)
            .is_some_and(|left| !left.is_zero())
        {
            return false;
        }
        self.retry.started();
        match open(paths) {
            Ok(source) => {
                self.source = Some(source);
                self.retry.reaped(Last::Ended, now);
                self.error = None;
                true
            }
            Err(error) => {
                self.failed(error, now, log);
                false
            }
        }
    }

    fn poll(&mut self, now: u64, log: &dyn Fn(&str)) -> bool {
        match self.source.as_mut().map(|source| source.poll()) {
            Some(Ok(arrived)) => arrived,
            Some(Err(error)) => {
                self.failed(error, now, log);
                false
            }
            None => false,
        }
    }
}

#[cfg(target_os = "macos")]
fn broker_arrived(
    broker: &mut aterm_uds::lan::Arrivals,
    last_error: &mut Option<String>,
    log: &(dyn Fn(&str) + Sync),
) -> bool {
    match broker.poll() {
        Ok(arrived) => {
            last_error.take();
            arrived
        }
        Err(error) => {
            let why = error.to_string();
            if last_error.as_ref() != Some(&why) {
                log(&format!(
                    "cannot watch the LAN broker's arrival: {why}; subscriber rounds still retry"
                ));
                *last_error = Some(why);
            }
            false
        }
    }
}

/// Ends the worker as `while_hearing` leaves, a panic included: its stop wakes the worker's
/// wait (its bell) and its retry's rest (a sleep on it).
struct Ending<'a>(&'a Stop);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.set();
    }
}

struct Notice<'a> {
    finishing: &'a AtomicBool,
    ending: &'a Stop,
    project: &'a dyn Projected,
}

impl Drop for Notice<'_> {
    fn drop(&mut self) {
        self.finishing.store(true, Ordering::Release);
        if !self.ending.is_set() {
            // A failed notification must not double-panic during an unwind.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                self.project.event(ProjectionEvent::Ended);
            }));
        }
    }
}

/// Observation's current owner and its existing retry deadline. A healthy listener
/// adds no wake deadline; missing/finishing custody cannot hide in an HTTP error nap.
pub(super) struct Status {
    pub(super) listening: bool,
    pub(super) retry: Option<(Instant, Duration)>,
}

impl Status {
    pub(super) fn limit(&self, pause: Duration) -> Duration {
        self.retry.map_or(pause, |(observed, retry)| {
            retry.saturating_sub(observed.elapsed()).min(pause)
        })
    }
}

struct Listener<C> {
    changed: C,
    switch: crate::swarm::Switch,
    held: bool,
    due: Option<Instant>,
    /// A fresh ask begun at or after this observed edge already covers it.
    edge: Option<Instant>,
    last: Option<Instant>,
    /// When a look is next owed with nothing heard: the projection's own deadline, or the end of
    /// the [`LOOK`] a ring's look waits for.
    look: Option<Instant>,
    /// When the worker last looked.
    looked: Option<Instant>,
    /// A ring, an exit or a deadline owes a look not yet taken ([`LOOK`] spaces looks).
    owed: bool,
    /// When the worker last found a member ask due by its pause.
    ask_checked: Option<Instant>,
    recovered: bool,
    arrivals: NativeArrivals,
    clock: Instant,
    #[cfg(target_os = "macos")]
    broker: aterm_uds::lan::Arrivals,
    #[cfg(target_os = "macos")]
    broker_error: Option<String>,
}

impl<C: Changes> Listener<C> {
    fn new(paths: &Paths, changed: C) -> Self {
        Self {
            changed,
            switch: crate::swarm::Switch::new(&paths.home),
            held: false,
            due: None,
            edge: None,
            last: None,
            look: Some(Instant::now() + LOOK),
            looked: None,
            owed: false,
            ask_checked: None,
            recovered: false,
            arrivals: NativeArrivals::default(),
            clock: Instant::now(),
            #[cfg(target_os = "macos")]
            broker: aterm_uds::lan::Arrivals::new(),
            #[cfg(target_os = "macos")]
            broker_error: None,
        }
    }

    fn reset(&mut self, paths: &Paths) {
        self.changed.reset();
        self.switch = crate::swarm::Switch::new(&paths.home);
        self.held = false;
        let now = Instant::now();
        self.due = Some(now);
        self.edge = Some(now);
        self.last = None;
        self.look = Some(now);
        self.looked = None;
        self.owed = false;
        self.ask_checked = None;
        self.recovered = true;
        self.arrivals = NativeArrivals::default();
        #[cfg(target_os = "macos")]
        {
            self.broker = aterm_uds::lan::Arrivals::new();
            self.broker_error = None;
        }
    }

    fn maintain_arrivals(
        &mut self,
        paths: &Paths,
        callbacks: Option<&super::MemberCallbacks>,
        asking: bool,
        refresh: bool,
        log: &dyn Fn(&str),
    ) -> bool {
        let Some(callbacks) = callbacks else {
            return false;
        };
        if !asking {
            self.arrivals = NativeArrivals::default();
            return false;
        }
        let was_held = self.held;
        if !swarm_lets(&mut self.switch, &mut self.held, "member discovery", log) {
            // An unreadable switch is a hold too. No dormant socket survives it.
            self.arrivals = NativeArrivals::default();
            return false;
        }
        let opened = self.arrivals.maintain(
            paths,
            &callbacks.arrivals,
            refresh,
            elapsed(self.clock),
            log,
        );
        if opened || was_held {
            // Listen before probing. A peer may already have announced itself
            // while the first attachment or the previous sharing episode was absent.
            let now = Instant::now();
            self.due = Some(now);
            self.edge = Some(now);
        }
        true
    }

    fn asking(&mut self, paths: &Paths, members: &Mutex<MembersAsked>, log: &dyn Fn(&str)) -> bool {
        let mut members = members.lock().unwrap_or_else(PoisonError::into_inner);
        if members.at.is_none() || members.on.is_none() {
            return false;
        }
        let was_held = self.held;
        if !swarm_lets(&mut self.switch, &mut self.held, "member discovery", log)
            || !members.still_asking(paths)
        {
            return false;
        }
        if was_held {
            let now = Instant::now();
            self.due = Some(now);
            self.edge = Some(now);
        }
        true
    }

    /// When a member ask is next due by its pause: an ask the worker already found due and could
    /// not make (a hold, a wait that ended) is looked at again a [`LOOK`] on, never at once.
    fn ask_at(&self, members: &MembersAsked) -> Option<Instant> {
        let at = members.at? + ask_members_pause(members.asks);
        Some(
            self.ask_checked
                .map_or(at, |checked| at.max(checked + LOOK)),
        )
    }

    /// When the worker next owes a round with nothing heard: a member ask's edge or pause, the
    /// native arrivals' retry while asking, and a look.
    fn deadline(&self, members: &Mutex<MembersAsked>, asking: bool) -> Option<Instant> {
        let now = Instant::now();
        let retry = if asking && self.arrivals.source.is_none() {
            self.arrivals
                .retry
                .remaining(elapsed(self.clock))
                .map(|left| now + left)
        } else {
            None
        };
        let ask = self.ask_at(&members.lock().unwrap_or_else(PoisonError::into_inner));
        [self.due, self.look, retry, ask]
            .into_iter()
            .flatten()
            .min()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the asking it reads, the projection it looks for, the doorbells it hears and its end"
    )]
    fn round(
        &mut self,
        paths: &Paths,
        members: &Mutex<MembersAsked>,
        ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
        log: &(dyn Fn(&str) + Sync),
        project: &dyn Projected,
        rings: Rings<'_>,
        ending: &Stop,
    ) -> std::io::Result<()> {
        if ending.is_set() {
            return Ok(());
        }
        // The HTTP thread rings as its asking begins, ends or moves; this round's reads take it.
        if let Some(bell) = rings.members {
            bell.clear();
        }
        // A runner another thread started or reaped: this round waits on its exit, and its next
        // look, spaced as every look is, joins the deadline.
        if rings.projection.is_some_and(Doorbell::clear)
            && let Some(at) = project.next_look()
        {
            let at = self.looked.map_or(at, |looked| at.max(looked + LOOK));
            self.look = Some(self.look.map_or(at, |look| look.min(at)));
        }
        let callbacks = super::ASK_MEMBERS.get();
        let asking = callbacks.is_some() && self.asking(paths, members, log);
        self.maintain_arrivals(paths, callbacks, asking, false, log);
        // Do not consume a broker generation while this round cannot observe routes.
        // The ordinary retry owns reopening, and reset owes one current observation
        // because a move may already have happened before the subscription returned.
        self.changed.check()?;
        let deadline = self.deadline(members, asking);
        #[cfg(target_os = "macos")]
        let before = broker_arrived(&mut self.broker, &mut self.broker_error, log);
        #[cfg(target_os = "macos")]
        let deadline = if before {
            Some(Instant::now())
        } else {
            match self.broker.wait_for(Duration::MAX) {
                Duration::MAX => deadline,
                left => deadline
                    .into_iter()
                    .chain(Instant::now().checked_add(left))
                    .min(),
            }
        };
        // A look already owed waits for its time, not on the exit that owes it: that exit reads
        // ready until the look reaps the runner.
        let wakes = if self.owed {
            Vec::new()
        } else {
            project.wakes()
        };
        let route = {
            let mut fds = self
                .arrivals
                .source
                .as_ref()
                .map_or_else(Vec::new, |source| source.fds());
            if fds.len() > 8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "native member readiness exceeds its attachment bound",
                ));
            }
            #[cfg(target_os = "macos")]
            if let Some(broker) = self.broker.watch_fd() {
                fds.push(broker);
            }
            // The stop's bell ends a wait that polls it as the subscriber ends. A bell this process
            // cannot make fails the round, which rests on the stop and retries.
            fds.push(ending.bell()?);
            fds.extend(rings.inputs.map(Doorbell::fd));
            fds.extend(rings.members.map(Doorbell::fd));
            fds.extend(rings.projection.map(Doorbell::fd));
            fds.extend(wakes.iter().map(|fd| fd.as_fd()));
            self.changed.wait(deadline, &fds)
        };
        #[cfg(target_os = "macos")]
        let broker_ready = before | broker_arrived(&mut self.broker, &mut self.broker_error, log);
        #[cfg(not(target_os = "macos"))]
        let broker_ready = false;
        if ending.is_set() {
            return Ok(());
        }
        let asking = callbacks.is_some() && self.asking(paths, members, log);
        let native_ready =
            self.maintain_arrivals(paths, callbacks, asking, route || broker_ready, log)
                && self.arrivals.poll(elapsed(self.clock), log);
        if route || broker_ready || native_ready {
            let now = Instant::now();
            self.edge = Some(now);
            self.due
                .get_or_insert_with(|| self.last.map_or(now, |at| (at + REST).max(now)));
        }
        if broker_ready {
            project.event(ProjectionEvent::BrokerReady);
        }
        // The observer and the projection's maintenance look in the round a change or an
        // arrival ends. A ring of the inputs, the runner's exit and the projection's own deadline
        // owe a look too, taken at most once a LOOK. A wake that brought none of them (the
        // members' ring, a wait that ended early) looks at nothing: each look reads files and the
        // routing table. A reset looks at once.
        let now = Instant::now();
        let rung = rings.inputs.is_some_and(Doorbell::clear);
        let exited = {
            let fds: Vec<_> = wakes.iter().map(|fd| fd.as_fd()).collect();
            !fds.is_empty() && aterm_uds::ready::any_now(&fds).is_ok_and(|ready| !ready.is_empty())
        };
        self.owed |= rung || exited || self.look.is_some_and(|at| now >= at);
        let spaced = self.looked.is_none_or(|at| now >= at + LOOK);
        if route || broker_ready || native_ready || (self.owed && spaced) {
            self.looked = Some(now);
            self.owed = false;
            project.event(ProjectionEvent::Maintenance);
            // The projection owns its sharing observation and retained activation debt.
            // Dispatch while Off too; it returns before capturing private readiness.
            if self.recovered {
                project.event(ProjectionEvent::Recovered);
                self.recovered = false;
            } else {
                project.event(ProjectionEvent::Observe);
            }
            // What the look moved is read here, not heard again. A deadline sooner than the
            // spacing waits for it, rather than waking to wait again.
            if let Some(bell) = rings.projection {
                bell.clear();
            }
            self.look = project.next_look().map(|at| at.max(now + LOOK));
        } else if self.owed {
            self.look = self.looked.map(|at| at + LOOK);
        }
        let changed = self.due.is_some_and(|at| at <= Instant::now());
        let ask_due = self
            .ask_at(&members.lock().unwrap_or_else(PoisonError::into_inner))
            .is_some_and(|at| at <= now);
        if ask_due {
            self.ask_checked = Some(now);
        }
        if !changed && !ask_due {
            return Ok(());
        }
        // The round and route worker share both the backoff and the actual ask.
        // Re-read sharing after waiting for a round's bounded proof to finish.
        let mut members = members.lock().unwrap_or_else(PoisonError::into_inner);
        let forced = changed
            && self
                .edge
                .is_none_or(|edge| members.at.is_none_or(|at| at < edge));
        if changed {
            self.due = None;
            self.edge = None;
        }
        if !forced
            && !members
                .at
                .is_some_and(|at| at.elapsed() >= ask_members_pause(members.asks))
        {
            return Ok(());
        }
        if !ending.is_set() && swarm_lets(&mut self.switch, &mut self.held, "member discovery", log)
        {
            if forced {
                members.at = None;
                members.asks = 0;
            }
            let before = admitted(paths);
            ask_members_if_due(paths, &mut members, ask, log);
            let progressed = !before && admitted(paths);
            drop(members);
            if progressed && swarm_lets(&mut self.switch, &mut self.held, "member convergence", log)
            {
                project.event(ProjectionEvent::Admitted);
            }
        }
        self.last = Some(Instant::now());
        Ok(())
    }
}

#[derive(Default)]
struct Owner<'scope> {
    worker: Option<ScopedJoinHandle<'scope, ()>>,
    retry: RunnerRetry,
    recovering: bool,
    finishing: Arc<AtomicBool>,
}

impl<'scope> Owner<'scope> {
    fn finish(&mut self, ending: &Stop) -> bool {
        ending.set();
        self.worker
            .take()
            .is_none_or(|worker| worker.join().is_ok())
    }

    fn maintain_with(
        &mut self,
        now: u64,
        ending: bool,
        start: impl FnOnce(bool) -> std::io::Result<ScopedJoinHandle<'scope, ()>>,
    ) -> std::io::Result<bool> {
        if self
            .worker
            .as_ref()
            .is_some_and(ScopedJoinHandle::is_finished)
        {
            let panicked = self
                .worker
                .take()
                .expect("finished listener")
                .join()
                .is_err();
            self.retry.reaped(Last::Failed, now);
            self.recovering = true;
            if !ending {
                return Err(std::io::Error::other(if panicked {
                    "member readiness listener panicked; retaining its retry"
                } else {
                    "member readiness listener ended; retaining its retry"
                }));
            }
        }
        if ending {
            return Ok(false);
        }
        if self.worker.is_some() {
            return Ok(!self.finishing.load(Ordering::Acquire));
        }
        if self
            .retry
            .remaining(now)
            .is_some_and(|left| !left.is_zero())
        {
            return Ok(false);
        }
        self.retry.started();
        self.finishing.store(false, Ordering::Release);
        match start(self.recovering) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.recovering = false;
                Ok(true)
            }
            Err(error) => {
                self.retry.reaped(Last::Failed, now);
                self.recovering = true;
                Err(error)
            }
        }
    }
}

fn elapsed(clock: Instant) -> u64 {
    u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Run `round` until `ending` is set, resting on the stop through each retry's whole pause after
/// a round that failed or panicked: a rest ends at its retry or as the worker ends, never at a
/// look of its own.
fn retry_rounds(
    clock: Instant,
    ending: &Stop,
    log: &(dyn Fn(&str) + Sync),
    mut round: impl FnMut() -> std::io::Result<()>,
    mut reset: impl FnMut(),
) {
    let mut retry = RunnerRetry::default();
    let mut last_error = None;
    while !ending.is_set() {
        if let Some(left) = retry
            .remaining(elapsed(clock))
            .filter(|left| !left.is_zero())
        {
            ending.sleep(left);
            continue;
        }
        retry.started();
        let why = match catch_unwind(AssertUnwindSafe(&mut round)) {
            Ok(Ok(())) => {
                retry.reaped(Last::Ended, elapsed(clock));
                last_error = None;
                continue;
            }
            Ok(Err(error)) => {
                format!(
                    "member readiness source unavailable: {error}; retrying with fresh observations"
                )
            }
            Err(_) => "member readiness round panicked; retrying with fresh observations".into(),
        };
        reset();
        retry.reaped(Last::Failed, elapsed(clock));
        if last_error.as_ref() != Some(&why) {
            log(&why);
            last_error = Some(why);
        }
    }
}

/// Own the readiness worker for exactly `run`'s lifetime. Event bursts are coalesced;
/// the first event is immediate and later events never create concurrent asks.
#[expect(
    clippy::too_many_arguments,
    reason = "the asking, projection, doorbells and route changes its worker hears, and the run it lasts"
)]
pub(super) fn while_hearing<T>(
    paths: &Paths,
    members: &Mutex<MembersAsked>,
    ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
    log: &(dyn Fn(&str) + Sync),
    project: &dyn Projected,
    rings: Rings<'_>,
    changed: impl Changes,
    run: impl FnOnce(&mut dyn FnMut() -> Status) -> T,
) -> T {
    let ending = Stop::new();
    let listener = Mutex::new(Listener::new(paths, changed));
    let clock = Instant::now();
    std::thread::scope(|scope| {
        let _ending = Ending(&ending);
        let ending = &ending;
        let listener = &listener;
        let mut owner = Owner::default();
        let mut maintain = || {
            let finishing = Arc::clone(&owner.finishing);
            let listening = owner
                .maintain_with(elapsed(clock), ending.is_set(), |recovering| {
                    std::thread::Builder::new()
                        .name("vault-member-routes".into())
                        .spawn_scoped(scope, move || {
                            let _notice = Notice {
                                finishing: &finishing,
                                ending,
                                project,
                            };
                            if recovering {
                                listener
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .reset(paths);
                                listener.clear_poison();
                            }
                            retry_rounds(
                                clock,
                                ending,
                                log,
                                || {
                                    listener
                                        .lock()
                                        .unwrap_or_else(PoisonError::into_inner)
                                        .round(paths, members, ask, log, project, rings, ending)
                                },
                                || {
                                    listener
                                        .lock()
                                        .unwrap_or_else(PoisonError::into_inner)
                                        .reset(paths);
                                    listener.clear_poison();
                                },
                            );
                        })
                })
                .unwrap_or_else(|error| {
                    log(&format!(
                        "member readiness listener waits: {error}; subscriber rounds keep observing"
                    ));
                    false
                });
            Status {
                listening,
                retry: (!listening).then(|| {
                    let observed = Instant::now();
                    let left = owner
                        .retry
                        .remaining(elapsed(clock))
                        .unwrap_or(Duration::from_millis(crate::converge::FIRST_RETRY_MS));
                    (observed, left)
                }),
            }
        };
        maintain();
        let result = run(&mut maintain);
        if !owner.finish(ending) {
            log("member readiness listener panicked before subscriber shutdown; joined it");
        }
        result
    })
}

fn admitted(paths: &Paths) -> bool {
    super::config(paths)
        .ok()
        .flatten()
        .is_some_and(|config| super::alab_vault(&config))
        && super::load_state(paths).is_ok_and(|state| state.membership == super::Stored::In)
        && !super::seat_ask::asks_for_seat(paths)
}

#[cfg(test)]
#[path = "member_routes_wait_tests.rs"]
mod wait_tests;

#[cfg(test)]
#[path = "member_routes_tests.rs"]
mod tests;
