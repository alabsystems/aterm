// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending subscriber can reach a member when its LAN changes or its broker arrives,
//! even while its vault request is held in a long poll. This worker invokes the authenticated
//! member callback; it provides no discovery, proof or admission authority of its own.

use super::{MembersAsked, Paths, ask_members_if_due, swarm_lets};
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
/// early with none (`also` ready); the worker then runs no round for it.
pub(super) trait Changes: Send {
    fn wait(&mut self, most: Duration, also: Option<BorrowedFd<'_>>) -> bool;
    fn reset(&mut self) {}
}

#[cfg(test)]
impl<F: FnMut(Duration) -> bool + Send> Changes for F {
    fn wait(&mut self, most: Duration, _also: Option<BorrowedFd<'_>>) -> bool {
        self(most)
    }
}

pub(super) struct NativeChanges {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    route: Option<crate::network::RouteChanges>,
}

impl NativeChanges {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            route: crate::network::RouteChanges::open().ok(),
        }
    }
}

impl Changes for NativeChanges {
    fn reset(&mut self) {
        *self = Self::new();
    }

    /// A change of network ends the wait as one; `also`'s readiness and the deadline end it as
    /// none. Messages that name no change never end it
    /// ([`crate::network::RouteChanges::wait_beside`]). When the route socket or poll(2) fails,
    /// the rest of the wait is spent on `also` alone, as a failed socket would poll ready again
    /// at once; the next wait hears the socket again, so a failure that passes (a read's error,
    /// a poll(2) that could not allocate) never leaves the worker deaf to later changes.
    fn wait(&mut self, most: Duration, also: Option<BorrowedFd<'_>>) -> bool {
        let deadline = Instant::now().checked_add(most);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(route) = &self.route {
            match route.wait_beside(most, also) {
                aterm_uds::network::RouteWake::Change => return true,
                aterm_uds::network::RouteWake::Also | aterm_uds::network::RouteWake::Timeout => {
                    return false;
                }
                aterm_uds::network::RouteWake::Failed => {}
            }
        }
        let Some(deadline) = deadline else {
            std::thread::sleep(most);
            return false;
        };
        match also {
            Some(fd) if aterm_uds::ready::any_until(&[fd], deadline).is_ok() => {}
            _ => std::thread::sleep(deadline.saturating_duration_since(Instant::now())),
        }
        false
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
    last: Option<Instant>,
    /// When the observer next looks with no change or arrival ([`LOOK`] after its last look).
    look: Instant,
    recovered: bool,
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
            last: None,
            look: Instant::now() + LOOK,
            recovered: false,
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
        self.due = Some(Instant::now());
        self.last = None;
        self.look = Instant::now();
        self.recovered = true;
        #[cfg(target_os = "macos")]
        {
            self.broker = aterm_uds::lan::Arrivals::new();
            self.broker_error = None;
        }
    }

    fn round(
        &mut self,
        paths: &Paths,
        members: &Mutex<MembersAsked>,
        ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
        log: &(dyn Fn(&str) + Sync),
        project: &(dyn Fn(ProjectionEvent) + Sync),
        ending: &AtomicBool,
    ) {
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
        #[cfg(target_os = "macos")]
        let route = self.changed.wait(wait, self.broker.watch_fd());
        #[cfg(not(target_os = "macos"))]
        let route = self.changed.wait(wait, None);
        #[cfg(target_os = "macos")]
        let broker_ready = before | broker_arrived(&mut self.broker, &mut self.broker_error, log);
        #[cfg(not(target_os = "macos"))]
        let broker_ready = false;
        if route || broker_ready {
            self.due.get_or_insert_with(|| {
                self.last
                    .map_or_else(Instant::now, |at| (at + REST).max(Instant::now()))
            });
        }
        if ending.load(Ordering::Acquire) {
            return;
        }
        if broker_ready {
            project(ProjectionEvent::BrokerReady);
        }
        // The observer and the projection's maintenance look at a change, at an arrival and at
        // their own pace, never at a wake that brought none of them: each look reads files and
        // the routing table, and a wait that ends early must not turn into a round. A reset
        // looks at once.
        let now = Instant::now();
        if route || broker_ready || now >= self.look {
            self.look = now + LOOK;
            project(ProjectionEvent::Maintenance);
            if swarm_lets(&mut self.switch, &mut self.held, "member convergence", log) {
                if self.recovered {
                    project(ProjectionEvent::Recovered);
                    self.recovered = false;
                } else {
                    project(ProjectionEvent::Observe);
                }
            }
        }
        if self.due.is_none_or(|at| at > Instant::now()) {
            return;
        }
        self.due = None;
        // The round and route worker share both the backoff and the actual ask.
        // Re-read sharing after waiting for a round's bounded proof to finish.
        let mut members = members.lock().unwrap_or_else(PoisonError::into_inner);
        if !ending.load(Ordering::Acquire)
            && swarm_lets(&mut self.switch, &mut self.held, "member discovery", log)
        {
            members.at = None;
            members.asks = 0;
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
    mut round: impl FnMut(),
    mut reset: impl FnMut(),
) {
    let mut retry = RunnerRetry::default();
    while !ending.load(Ordering::Acquire) {
        if let Some(left) = retry
            .remaining(elapsed(clock))
            .filter(|left| !left.is_zero())
        {
            std::thread::sleep(left.min(LOOK));
            continue;
        }
        retry.started();
        if catch_unwind(AssertUnwindSafe(&mut round)).is_ok() {
            retry.reaped(Last::Ended, elapsed(clock));
        } else {
            reset();
            retry.reaped(Last::Failed, elapsed(clock));
            log("member readiness round panicked; retrying with fresh observations");
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
                                            .round(paths, members, ask, log, project, ending);
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
            fn wait(&mut self, _: Duration, _: Option<BorrowedFd<'_>>) -> bool {
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
        let route = Some(crate::network::RouteChanges::over(heard.into()));
        (NativeChanges { route }, kernel)
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
        let changed = changes.wait(most, Some(quiet.as_fd()));
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
            route: Some(crate::network::RouteChanges::over(heard.into())),
        };
        let (quiet, _writer) = std::io::pipe().expect("a broker descriptor nobody writes");
        let most = Duration::from_millis(300);
        for _ in 0..2 {
            let began = Instant::now();
            assert!(!changes.wait(most, Some(quiet.as_fd())));
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
            route: Some(crate::network::RouteChanges::over(heard.into())),
        };
        let (quiet, _writer) = std::io::pipe().expect("a broker descriptor nobody writes");
        assert!(!changes.wait(Duration::from_millis(300), Some(quiet.as_fd())));
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
            changes.wait(HANG, Some(quiet.as_fd())),
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
            !changes.wait(HANG, Some(ready.as_fd())),
            "an arrival is no change of network"
        );
        assert!(
            began.elapsed() < HANG,
            "a ready broker descriptor was waited out beside a route socket that hears"
        );
        let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
        drop(peer);
        let mut failed = NativeChanges {
            route: Some(crate::network::RouteChanges::over(heard.into())),
        };
        let began = Instant::now();
        assert!(
            !failed.wait(HANG, Some(ready.as_fd())),
            "an arrival is no change of network"
        );
        assert!(
            began.elapsed() < HANG,
            "a ready broker descriptor was waited out beside a failed route socket"
        );
    }
}
