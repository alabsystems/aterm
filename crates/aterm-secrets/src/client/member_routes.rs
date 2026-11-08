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
use std::os::fd::BorrowedFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ScopedJoinHandle;
use std::time::{Duration, Instant};

/// The pace of the local observer ([`ProjectionEvent::Observe`]) and the projection's
/// maintenance ([`ProjectionEvent::Maintenance`]) between changes and arrivals.
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

/// What the worker waits on: whether a change of network came within `most`. A wait may end
/// early with none (`also` ready); the worker reads those sources separately.
pub(super) trait Changes: Send {
    /// Check retained observation custody before another readiness source is consumed.
    fn check(&self) -> std::io::Result<()> {
        Ok(())
    }
    fn wait(&mut self, most: Duration, also: &[BorrowedFd<'_>]) -> bool;
    fn reset(&mut self) {}
}

#[cfg(test)]
impl<F: FnMut(Duration) -> bool + Send> Changes for F {
    fn wait(&mut self, most: Duration, _also: &[BorrowedFd<'_>]) -> bool {
        self(most)
    }
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
    /// a poll(2) that could not allocate) never leaves the worker deaf to later changes.
    fn wait(&mut self, most: Duration, also: &[BorrowedFd<'_>]) -> bool {
        let immediate = most.is_zero();
        let Some(deadline) = Instant::now().checked_add(most) else {
            std::thread::sleep(most);
            return false;
        };
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
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                return false;
            }
            let ready = if immediate {
                let interests: Vec<_> = fds
                    .iter()
                    .map(|fd| aterm_uds::ready::Watch::new(*fd, aterm_uds::ready::Interest::Read))
                    .collect();
                aterm_uds::ready::any_interests_now(&interests)
            } else {
                aterm_uds::ready::any_until(&fds, deadline)
            };
            let Ok(ready) = ready else {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if route.take().is_some() {
                    // Retry only the retained additional descriptors, at the same
                    // deadline. A second poll failure is waited out, never spun.
                    continue;
                }
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
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

struct Ending<'a>(&'a AtomicBool);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Notice<'a> {
    finishing: &'a AtomicBool,
    ending: &'a AtomicBool,
    project: &'a (dyn Fn(ProjectionEvent) + Sync),
}

impl Drop for Notice<'_> {
    fn drop(&mut self) {
        self.finishing.store(true, Ordering::Release);
        if !self.ending.load(Ordering::Acquire) {
            // A failed notification must not double-panic during an unwind.
            let _ = catch_unwind(AssertUnwindSafe(|| (self.project)(ProjectionEvent::Ended)));
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
    /// When the observer next looks with no change or arrival ([`LOOK`] after its last look).
    look: Instant,
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
            look: Instant::now() + LOOK,
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
        self.look = Instant::now();
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

    fn round(
        &mut self,
        paths: &Paths,
        members: &Mutex<MembersAsked>,
        ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
        log: &(dyn Fn(&str) + Sync),
        project: &(dyn Fn(ProjectionEvent) + Sync),
        ending: &AtomicBool,
    ) -> std::io::Result<()> {
        if ending.load(Ordering::Acquire) {
            return Ok(());
        }
        let callbacks = super::ASK_MEMBERS.get();
        let asking = callbacks.is_some() && self.asking(paths, members, log);
        self.maintain_arrivals(paths, callbacks, asking, false, log);
        // Do not consume a broker generation while this round cannot observe routes.
        // The ordinary retry owns reopening, and reset owes one current observation
        // because a move may already have happened before the subscription returned.
        self.changed.check()?;
        let wait = self
            .due
            .map_or(self.look, |at| at.min(self.look))
            .saturating_duration_since(Instant::now());
        #[cfg(target_os = "macos")]
        let before = broker_arrived(&mut self.broker, &mut self.broker_error, log);
        #[cfg(target_os = "macos")]
        let wait = if before {
            Duration::ZERO
        } else {
            self.broker.wait_for(wait)
        };
        let route = {
            let fds = self
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
            let fds = {
                let mut fds = fds;
                if let Some(broker) = self.broker.watch_fd() {
                    fds.push(broker);
                }
                fds
            };
            self.changed.wait(wait, &fds)
        };
        #[cfg(target_os = "macos")]
        let broker_ready = before | broker_arrived(&mut self.broker, &mut self.broker_error, log);
        #[cfg(not(target_os = "macos"))]
        let broker_ready = false;
        if ending.load(Ordering::Acquire) {
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
            project(ProjectionEvent::BrokerReady);
        }
        // The observer and the projection's maintenance look at a change, at an arrival and at
        // their own pace, never at a wake that brought none of them: each look reads files and
        // the routing table, and a wait that ends early must not turn into a round. A reset
        // looks at once.
        let now = Instant::now();
        let periodic = now >= self.look;
        if route || broker_ready || native_ready || periodic {
            self.look = now + LOOK;
            project(ProjectionEvent::Maintenance);
            // The projection owns its sharing observation and retained activation debt.
            // Dispatch while Off too; it returns before capturing private readiness.
            if self.recovered {
                project(ProjectionEvent::Recovered);
                self.recovered = false;
            } else {
                project(ProjectionEvent::Observe);
            }
        }
        let changed = self.due.is_some_and(|at| at <= Instant::now());
        if !changed && !periodic {
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
        if !ending.load(Ordering::Acquire)
            && swarm_lets(&mut self.switch, &mut self.held, "member discovery", log)
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
                project(ProjectionEvent::Admitted);
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
    fn finish(&mut self, ending: &AtomicBool) -> bool {
        ending.store(true, Ordering::Release);
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

fn retry_rounds(
    clock: Instant,
    ending: &AtomicBool,
    log: &(dyn Fn(&str) + Sync),
    mut round: impl FnMut() -> std::io::Result<()>,
    mut reset: impl FnMut(),
) {
    let mut retry = RunnerRetry::default();
    let mut last_error = None;
    while !ending.load(Ordering::Acquire) {
        if let Some(left) = retry
            .remaining(elapsed(clock))
            .filter(|left| !left.is_zero())
        {
            std::thread::sleep(left.min(LOOK));
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
pub(super) fn while_hearing<T>(
    paths: &Paths,
    members: &Mutex<MembersAsked>,
    ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
    log: &(dyn Fn(&str) + Sync),
    project: &(dyn Fn(ProjectionEvent) + Sync),
    changed: impl Changes,
    run: impl FnOnce(&mut dyn FnMut() -> Status) -> T,
) -> T {
    let ending = AtomicBool::new(false);
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
                .maintain_with(
                    elapsed(clock),
                    ending.load(Ordering::Acquire),
                    |recovering| {
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
                                            .round(paths, members, ask, log, project, ending)
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
                    },
                )
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
mod wait_tests {
    use super::*;

    #[test]
    fn refused_or_ended_listeners_keep_one_retry_and_do_not_restart_after_ending() {
        let bell = crate::wake::Bell::default();
        let ending = AtomicBool::new(false);
        let project = |event| {
            assert!(matches!(event, ProjectionEvent::Ended));
            bell.ring(crate::wake::Wake::Listener);
        };
        std::thread::scope(|scope| {
            let mut owner = Owner::default();
            assert!(
                owner
                    .maintain_with(0, false, |recovering| {
                        assert!(!recovering);
                        Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            "injected spawn refusal",
                        ))
                    })
                    .is_err()
            );
            let first = crate::converge::FIRST_RETRY_MS;
            assert!(
                !owner
                    .maintain_with(first - 1, false, |_| panic!("retry not due"))
                    .unwrap()
            );
            assert!(
                owner
                    .maintain_with(first, false, |recovering| {
                        assert!(recovering);
                        std::thread::Builder::new()
                            .spawn_scoped(scope, || panic!("injected outer worker failure"))
                    })
                    .unwrap()
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while !owner.worker.as_ref().unwrap().is_finished() {
                assert!(Instant::now() < deadline, "listener did not finish");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                owner
                    .maintain_with(first, false, |_| panic!("failure must be reaped first"))
                    .is_err()
            );
            assert!(owner.worker.is_none(), "the failed handle was joined");
            assert!(
                !owner
                    .maintain_with(first * 3 - 1, false, |_| panic!("retry not due"))
                    .unwrap()
            );
            let (finish, finishing) = std::sync::mpsc::channel();
            let (noticed, noticing) = std::sync::mpsc::channel();
            let (pausing, paused) = std::sync::mpsc::channel();
            bell.ring(crate::wake::Wake::Route);
            let pause_bell = &bell;
            let pause = scope.spawn(move || {
                pausing.send(()).unwrap();
                super::super::pause_on_bell(pause_bell, Duration::from_secs(60))
            });
            paused.recv_timeout(Duration::from_secs(60)).unwrap();
            let notice_flag = Arc::clone(&owner.finishing);
            let ending = &ending;
            let project = &project;
            assert!(
                owner
                    .maintain_with(first * 3, false, |recovering| {
                        assert!(recovering);
                        std::thread::Builder::new().spawn_scoped(scope, move || {
                            drop(Notice {
                                finishing: &notice_flag,
                                ending,
                                project,
                            });
                            noticed.send(()).unwrap();
                            finishing.recv_timeout(Duration::from_secs(60)).unwrap();
                        })
                    })
                    .unwrap()
            );
            noticing.recv_timeout(Duration::from_secs(60)).unwrap();
            assert_eq!(pause.join().unwrap(), crate::wake::Wake::Listener);
            assert!(
                !owner
                    .maintain_with(first * 3, false, |_| panic!("one listener at a time"))
                    .unwrap(),
                "the exit notice precedes the finished handle"
            );
            assert!(
                !owner
                    .maintain_with(first * 3, true, |_| panic!("ending starts no listener"))
                    .unwrap()
            );
            finish.send(()).unwrap();
            assert!(owner.finish(ending));
            assert!(
                !owner
                    .maintain_with(first * 10, true, |_| panic!("no restart after ending"))
                    .unwrap()
            );
        });
        struct PanicsWhileResetting;
        impl Changes for PanicsWhileResetting {
            fn wait(&mut self, _: Duration, _: &[BorrowedFd<'_>]) -> bool {
                panic!("injected readiness failure before shutdown");
            }
            fn reset(&mut self) {
                panic!("unreaped outer failure at shutdown");
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let members = Mutex::new(MembersAsked::default());
        let (ended, ending) = std::sync::mpsc::channel();
        let returned = while_hearing(
            &paths,
            &members,
            &|_| panic!("failed listener admits no member work"),
            &|_| {},
            &|event| {
                if matches!(event, ProjectionEvent::Ended) {
                    ended.send(()).unwrap();
                }
            },
            PanicsWhileResetting,
            |_| {
                ending.recv_timeout(Duration::from_secs(60)).unwrap();
                // Return without another maintenance call or a manual join. The
                // actual owner must consume the panic before scope autojoin.
                17
            },
        );
        assert_eq!(returned, 17);

        struct Inert(Arc<std::sync::atomic::AtomicUsize>);
        impl MemberArrivals for Inert {
            fn refresh(&mut self) -> std::io::Result<()> {
                Ok(())
            }
            fn fds(&self) -> Vec<BorrowedFd<'_>> {
                Vec::new()
            }
            fn poll(&mut self) -> std::io::Result<bool> {
                Ok(false)
            }
        }
        impl Drop for Inert {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let opened = std::sync::atomic::AtomicUsize::new(0);
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let open = |_: &Paths| -> std::io::Result<Box<dyn MemberArrivals>> {
            if opened.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(std::io::Error::from_raw_os_error(libc::EMFILE));
            }
            Ok(Box::new(Inert(Arc::clone(&dropped))))
        };
        let first = crate::converge::FIRST_RETRY_MS;
        let mut native = NativeArrivals::default();
        assert!(!native.maintain(&paths, &open, false, 0, &|_| {}));
        assert!(!native.maintain(&paths, &open, false, first - 1, &|_| {}));
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        assert!(native.maintain(&paths, &open, false, first, &|_| {}));
        assert!(!native.maintain(&paths, &open, false, first * 100, &|_| {}));
        assert_eq!(
            opened.load(Ordering::SeqCst),
            2,
            "healthy custody is retained"
        );
        assert!(!native.poll(first * 100, &|_| {}));

        let config = paths.home.join(".config/aterm/aterm.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = false\n");
        let forbidden = super::super::MemberCallbacks {
            ask: |_| panic!("readiness never authenticates a member"),
            arrivals: |_| panic!("held or healthy custody opens no new source"),
        };
        let mut listener = Listener::new(&paths, |_: Duration| false);
        listener.arrivals = native;
        assert!(!listener.maintain_arrivals(&paths, Some(&forbidden), true, false, &|_| {}));
        assert!(listener.arrivals.source.is_none());
        assert_eq!(dropped.load(Ordering::SeqCst), 1, "Off releases the owner");
        crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = true\n");
        let permitted = super::super::MemberCallbacks {
            ask: forbidden.ask,
            arrivals: |_| {
                Ok(Box::new(Inert(Arc::new(
                    std::sync::atomic::AtomicUsize::new(0),
                ))))
            },
        };
        assert!(listener.maintain_arrivals(&paths, Some(&permitted), true, false, &|_| {}));
        assert!(listener.arrivals.source.is_some());
        assert!(listener.due.is_some(), "On owes a fresh ordinary ask");
        assert!(listener.maintain_arrivals(&paths, Some(&forbidden), true, false, &|_| {}));
        assert!(!listener.maintain_arrivals(&paths, Some(&forbidden), false, false, &|_| {}));
        assert!(
            listener.arrivals.source.is_none(),
            "asking ended before a new read"
        );
        assert!(listener.maintain_arrivals(&paths, Some(&permitted), true, false, &|_| {}));
        crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = 'unknown'\n");
        assert!(!listener.maintain_arrivals(&paths, Some(&forbidden), true, false, &|_| {}));
        assert!(
            listener.arrivals.source.is_none(),
            "unreadable policy holds too"
        );

        aterm_uds::lan::seam_here(
            aterm_uds::lan::Seam {
                socket: Some(paths.home.join("absent-broker.sock")),
                ..aterm_uds::lan::Seam::default()
            },
            || {
                let edge = Instant::now();
                let started = Instant::now();
                let members = Mutex::new(MembersAsked {
                    at: Some(started),
                    asks: 1,
                    ..MembersAsked::default()
                });
                let mut listener = Listener::new(&paths, |_: Duration| false);
                listener.due = Some(edge);
                listener.edge = Some(edge);
                listener.look = started + Duration::from_secs(60);
                // The main round won the shared ask after this observed edge.
                // Servicing it must not reset the completed ask or read private
                // waiting state again, even though the callback itself is current.
                listener
                    .round(
                        &paths,
                        &members,
                        &|_| panic!("this edge is already covered by the fresh ask"),
                        &|_| {},
                        &|_| panic!("no new observation is due"),
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                let asked = members.lock().unwrap();
                assert_eq!(asked.at, Some(started));
                assert_eq!(asked.asks, 1);
                assert!(listener.due.is_none());
                assert!(listener.edge.is_none());
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A wait for something that must happen: a hang detector, never a latency budget.
    const HANG: Duration = Duration::from_secs(60);

    /// How long the worker runs while its waits end early: longer than one look.
    const WINDOW: Duration = Duration::from_millis(1500);

    /// Run the worker over `changed`, under a private broker socket, until
    /// `enough(observed, waits, ran)` holds ([`HANG`] detects one that never does): how many
    /// times it observed, how many times it waited, and how long it ran. `changed` hears each
    /// wait's length and how many times the worker had observed when it began. With `arrives`,
    /// a broker answers at that socket from the start (macOS, where the worker watches for one);
    /// without, none ever comes.
    fn observes_over(
        arrives: bool,
        mut changed: impl FnMut(Duration, usize) -> bool + Send,
        enough: impl Fn(usize, usize, Duration) -> bool,
    ) -> (usize, usize, Duration) {
        let home = aterm_tempfile::tempdir().expect("home");
        let paths = Paths::locate_for_home(home.path());
        let members = Mutex::new(MembersAsked::default());
        let observed = AtomicUsize::new(0);
        let waits = AtomicUsize::new(0);
        let project = |event: ProjectionEvent| {
            if matches!(event, ProjectionEvent::Observe) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        };
        let changed = |most: Duration| {
            waits.fetch_add(1, Ordering::SeqCst);
            changed(most, observed.load(Ordering::SeqCst))
        };
        let began = Instant::now();
        let hear = || {
            while_hearing(
                &paths,
                &members,
                &|_: &Paths| Ok(0),
                &|_| {},
                &project,
                changed,
                |_: &mut dyn FnMut() -> Status| {
                    while !enough(
                        observed.load(Ordering::SeqCst),
                        waits.load(Ordering::SeqCst),
                        began.elapsed(),
                    ) {
                        assert!(began.elapsed() < HANG, "the worker never got there");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                },
            );
        };
        #[cfg(target_os = "macos")]
        {
            let socket = home.path().join("broker.sock");
            std::thread::scope(|scope| {
                let broker = arrives.then(|| {
                    let listener =
                        std::os::unix::net::UnixListener::bind(&socket).expect("a private broker");
                    scope.spawn(move || answer_one_hello(&listener))
                });
                aterm_uds::lan::seam_here(
                    aterm_uds::lan::Seam {
                        socket: Some(socket.clone()),
                        ..aterm_uds::lan::Seam::default()
                    },
                    hear,
                );
                if let Some(broker) = broker {
                    broker.join().expect("the broker answered");
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(
                !arrives,
                "a broker arrives only where the worker watches for one"
            );
            hear();
        }
        (
            observed.load(Ordering::SeqCst),
            waits.load(Ordering::SeqCst),
            began.elapsed(),
        )
    }

    /// The most looks a worker that ran `ran` may take at its own pace: one per [`LOOK`], and
    /// one more for the look under way as it ended.
    fn paced(ran: Duration) -> usize {
        usize::try_from(ran.as_millis() / LOOK.as_millis()).unwrap_or(usize::MAX) + 1
    }

    /// A wait that ends early with no change and no broker (0.159.0 and 0.160.0's ended at each
    /// routing message, a millisecond in) runs no round: the observer keeps its own pace however
    /// often the worker wakes. The worker runs past [`WINDOW`] and until its waits outnumber its
    /// paced looks fourfold, so the premise holds however slowly the machine runs. A wait that
    /// runs its time still looks: the worker runs until it has.
    #[test]
    fn early_wakes_with_nothing_run_no_round() {
        let (observed, waits, ran) = observes_over(
            false,
            |most, _| {
                std::thread::sleep(most.min(Duration::from_millis(1)));
                false
            },
            |_, waits, ran| ran >= WINDOW && waits > 4 * paced(ran),
        );
        assert!(
            observed <= paced(ran),
            "{observed} looks in {ran:?} from {waits} early wakes"
        );
        let (observed, _, ran) = observes_over(
            false,
            |most, _| {
                std::thread::sleep(most);
                false
            },
            |observed, _, _| observed > 0,
        );
        assert!(
            observed <= paced(ran),
            "{observed} looks in {ran:?} at the observer's pace"
        );
    }

    /// Answer the first hello at `listener` as a broker does ([`HANG`] detects a worker that
    /// never dials).
    #[cfg(target_os = "macos")]
    fn answer_one_hello(listener: &std::os::unix::net::UnixListener) {
        use aterm_uds::lan::wire::{self, Frame, Reader};
        use std::os::fd::AsFd as _;
        let dialled = aterm_uds::ready::any_until(&[listener.as_fd()], Instant::now() + HANG)
            .expect("the broker's wait for a dial");
        assert!(!dialled.is_empty(), "the worker never dialled its broker");
        let (stream, _) = listener.accept().expect("the worker's dial");
        let hello = Reader::new(0)
            .frame(&stream, Some(Instant::now() + HANG))
            .expect("the worker's hello");
        assert!(matches!(hello, Some(Frame::Hello { .. })), "{hello:?}");
        wire::send(
            &stream,
            &Frame::Hello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "stand-in broker".into(),
            },
        )
        .expect("the broker answers");
    }

    /// How many times the worker had observed when its second wait began, its first wait having
    /// said `first` (with `arrives`, beside a broker that answers from the start).
    fn looks_before_the_second_wait(arrives: bool, first: bool) -> usize {
        let second = std::sync::OnceLock::new();
        let mut waits = 0_usize;
        observes_over(
            arrives,
            |most, looks| {
                waits += 1;
                match waits {
                    1 => first,
                    2 => {
                        let _ = second.set(looks);
                        false
                    }
                    _ => {
                        std::thread::sleep(most.min(Duration::from_millis(1)));
                        false
                    }
                }
            },
            |_, _, _| second.get().is_some(),
        );
        *second.get().expect("a second wait")
    }

    /// A change of network, and a broker's arrival, has the observer and the projection's
    /// maintenance look in the round it ends, before the worker waits again: neither waits for
    /// the observer's own pace ([`LOOK`]), which only a wake that brought neither keeps.
    #[test]
    fn a_change_or_an_arrival_looks_before_the_next_wait() {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsFd as _;
            use std::os::unix::net::UnixDatagram;
            let (route, routes) = UnixDatagram::pair().unwrap();
            let mut routes = Some(routes);
            let mut changes = NativeChanges::opening(move || {
                Ok(crate::network::RouteChanges::over(
                    routes.take().unwrap().into(),
                ))
            });
            let (broker, _broker_sender) = UnixDatagram::pair().unwrap();
            let (native, sender) = UnixDatagram::pair().unwrap();
            // A non-route kernel message cannot hide a ready native descriptor
            // beside the existing broker descriptor. The actual reader owns bytes.
            route.send(&[0; 24]).unwrap();
            sender.send(b"owned native readiness").unwrap();
            assert!(!changes.wait(HANG, &[broker.as_fd(), native.as_fd()]));
            let mut bytes = [0; 64];
            let read = native.recv(&mut bytes).unwrap();
            assert_eq!(&bytes[..read], b"owned native readiness");
        }
        assert_eq!(
            looks_before_the_second_wait(false, true),
            1,
            "a change of network waited for the observer's pace"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            looks_before_the_second_wait(true, false),
            1,
            "a broker's arrival waited for the observer's pace"
        );
    }

    /// The other end of a stand-in route socket: kernel event messages written to it reach the
    /// wait as `PF_SYSTEM`'s would.
    #[cfg(target_os = "macos")]
    fn stand_in() -> (NativeChanges, std::os::unix::net::UnixDatagram) {
        let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
        kernel
            .set_nonblocking(true)
            .expect("a writer that never blocks");
        let route = Ok(crate::network::RouteChanges::over(heard.into()));
        (
            NativeChanges {
                route,
                open: Box::new(crate::network::RouteChanges::open),
            },
            kernel,
        )
    }

    /// Kernel events that name no change of network, as fast as the stand-in takes them, never
    /// end the worker's wait early; the broker's descriptor beside it is quiet.
    #[cfg(target_os = "macos")]
    #[test]
    fn unrelated_network_events_never_end_the_native_wait() {
        use std::os::fd::AsFd as _;
        let (mut changes, kernel) = stand_in();
        let (quiet, _writer) = std::io::pipe().expect("a broker descriptor nobody writes");
        let flooding = std::sync::Arc::new(AtomicBool::new(true));
        let (flooded, floods) = std::sync::mpsc::channel();
        let (begun, beginning) = std::sync::mpsc::channel();
        let flood = {
            let flooding = std::sync::Arc::clone(&flooding);
            std::thread::spawn(move || {
                // KEV_NETWORK_CLASS, a subclass that names no address, router or link.
                let unrelated: Vec<u8> = [24_u32, 1, 1, 3, 37, 1]
                    .into_iter()
                    .flat_map(u32::to_ne_bytes)
                    .collect();
                let mut sent = 0_u64;
                while flooding.load(Ordering::Acquire) {
                    match kernel.send(&unrelated) {
                        Ok(_) => {
                            if sent == 0 {
                                let _ = begun.send(());
                            }
                            sent += 1;
                        }
                        // A full queue: macOS answers ENOBUFS for a local datagram socket.
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                || error.raw_os_error() == Some(libc::ENOBUFS) =>
                        {
                            aterm_uds::deadline::poll_yield();
                        }
                        Err(error) => panic!("the flood's send: {error}"),
                    }
                }
                let _ = flooded.send(sent);
            })
        };
        beginning.recv_timeout(HANG).expect("the flood begins");
        let most = Duration::from_millis(300);
        let began = Instant::now();
        let changed = changes.wait(most, &[quiet.as_fd()]);
        let waited = began.elapsed();
        flooding.store(false, Ordering::Release);
        let sent = floods.recv_timeout(HANG).expect("the flood ends");
        flood.join().expect("the flood ends cleanly");
        assert!(!changed, "{sent} unrelated messages are no change");
        assert!(
            waited >= most,
            "{sent} unrelated messages ended a {most:?} wait after {waited:?}"
        );
    }

    /// A wait whose route socket fails runs its time on the broker's descriptor alone, wait
    /// after wait; it never returns at once to be polled again.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_route_socket_is_waited_out() {
        use std::os::fd::AsFd as _;
        let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
        drop(peer);
        let mut changes = NativeChanges {
            route: Ok(crate::network::RouteChanges::over(heard.into())),
            open: Box::new(crate::network::RouteChanges::open),
        };
        let (quiet, _writer) = std::io::pipe().expect("a broker descriptor nobody writes");
        let most = Duration::from_millis(300);
        for _ in 0..2 {
            let began = Instant::now();
            assert!(!changes.wait(most, &[quiet.as_fd()]));
            assert!(
                began.elapsed() >= most,
                "a failed socket ended the wait after {:?}",
                began.elapsed()
            );
        }
    }

    /// A read that fails once (a refused datagram's error, which the read takes) costs the
    /// worker that one wait: the next wait hears the same socket, and a change on it ends that
    /// wait as one.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_failure_that_passes_never_deafens_the_worker() {
        use std::os::fd::AsFd as _;
        let loopback = std::net::Ipv4Addr::LOCALHOST;
        let closed = std::net::UdpSocket::bind((loopback, 0))
            .and_then(|socket| socket.local_addr())
            .expect("a port nobody holds");
        let heard = std::net::UdpSocket::bind((loopback, 0)).expect("the route stand-in");
        heard.connect(closed).expect("a peer that refuses");
        heard.send(&[0]).expect("a datagram the port refuses");
        let witness = heard.try_clone().expect("a second handle");
        assert!(
            aterm_uds::ready::any_until(&[witness.as_fd()], Instant::now() + HANG)
                .is_ok_and(|ready| !ready.is_empty()),
            "the premise: the refusal is waiting"
        );
        let mut changes = NativeChanges {
            route: Ok(crate::network::RouteChanges::over(heard.into())),
            open: Box::new(crate::network::RouteChanges::open),
        };
        let (quiet, _writer) = std::io::pipe().expect("a broker descriptor nobody writes");
        assert!(!changes.wait(Duration::from_millis(300), &[quiet.as_fd()]));
        assert!(
            witness.take_error().expect("the socket's error").is_none(),
            "the premise: the failed wait read the refusal"
        );
        let teller = std::net::UdpSocket::bind((loopback, 0)).expect("the kernel's stand-in");
        witness
            .connect(teller.local_addr().expect("the teller's address"))
            .expect("the stand-in hears the teller");
        let change: Vec<u8> = [24_u32, 1, 1, 1, 37, 1]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        teller
            .send_to(
                &change,
                witness.local_addr().expect("the stand-in's address"),
            )
            .expect("an address change told");
        assert!(
            changes.wait(HANG, &[quiet.as_fd()]),
            "a change after one failed read was not heard"
        );
    }

    /// The broker's descriptor ends the native wait when it is ready, as no change of network,
    /// whether the route socket beside it hears or has failed: the worker hears an arrival at
    /// once, never at the wait's end ([`HANG`] detects one waited out).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_ready_broker_descriptor_ends_the_native_wait() {
        use std::io::Write as _;
        use std::os::fd::AsFd as _;
        let (ready, mut ring) = std::io::pipe().expect("a broker descriptor");
        ring.write_all(&[1]).expect("the broker's arrival");
        let (mut changes, _kernel) = stand_in();
        let began = Instant::now();
        assert!(
            !changes.wait(HANG, &[ready.as_fd()]),
            "an arrival is no change of network"
        );
        assert!(
            began.elapsed() < HANG,
            "a ready broker descriptor was waited out beside a route socket that hears"
        );
        let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
        drop(peer);
        let mut failed = NativeChanges {
            route: Ok(crate::network::RouteChanges::over(heard.into())),
            open: Box::new(crate::network::RouteChanges::open),
        };
        let began = Instant::now();
        assert!(
            !failed.wait(HANG, &[ready.as_fd()]),
            "an arrival is no change of network"
        );
        assert!(
            began.elapsed() < HANG,
            "a ready broker descriptor was waited out beside a failed route socket"
        );

        let (mut queued, kernel) = stand_in();
        let change: Vec<u8> = [24_u32, 1, 1, 1, 37, 1]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        kernel.send(&change).unwrap();
        assert!(
            queued.wait(Duration::ZERO, &[ready.as_fd()]),
            "a queued route change is consumed even when another edge is already due"
        );
        assert!(
            !queued.wait(Duration::ZERO, &[ready.as_fd()]),
            "the route was drained and the broker still owns its unread bytes"
        );
        let unrelated: Vec<u8> = [24_u32, 1, 1, 3, 37, 1]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        kernel.send(&unrelated).unwrap();
        assert!(!queued.wait(Duration::ZERO, &[]));
    }
}
