// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use aterm_agent::harness::netwatch::{API_HOST, API_PORT};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::sync::mpsc;

const BUDGET: Duration = Duration::from_secs(2);

#[cfg(target_os = "macos")]
#[test]
fn a_local_endpoint_probe_resolves_through_the_lan_broker() {
    use aterm_uds::lan::{
        self,
        wire::{Frame, Reader, Request},
    };

    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("broker.sock");
    let listener = aterm_uds::CtlListener::bind(&socket).unwrap();
    let served = std::thread::spawn(move || {
        let hang = Duration::from_secs(60);
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + hang),
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        lan::wire::send(
            &stream,
            &Frame::Hello {
                protocol: lan::PROTOCOL,
                build: "probe-fixture".into(),
            },
        )
        .unwrap();
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + hang);
        assert!(matches!(
            reader.frame(&stream, deadline).unwrap(),
            Some(Frame::Hello { .. })
        ));
        assert!(matches!(reader.frame(&stream, deadline).unwrap(),
            Some(Frame::Request(Request::Resolve { name, .. })) if name == "probe-fixture.local"));
        lan::wire::send(
            &stream,
            &Frame::Addresses(vec!["192.0.2.17".parse().unwrap()]),
        )
        .unwrap();
    });
    let addresses = lan::seam_here(
        lan::Seam {
            socket: Some(socket),
            ..lan::Seam::default()
        },
        || SystemProbe.resolve("probe-fixture.local", 8443),
    )
    .unwrap();
    served.join().unwrap();
    assert_eq!(
        addresses,
        vec!["192.0.2.17:8443".parse::<SocketAddr>().unwrap()]
    );
}

/// Which publisher of a wake (a stop, or a path's nudge) the fixture runs: the shipping
/// mutex-guarded one, or the unlocked negative control.
#[derive(Clone, Copy)]
enum ShutdownPublisher {
    MutexGuarded,
    HistoricalUnlocked,
}

#[derive(Debug)]
struct ShutdownObservation {
    action: &'static str,
    stopped: bool,
}

impl ShutdownObservation {
    fn read(action: &'static str, probe: &NetProbe) -> Self {
        Self {
            action,
            stopped: probe.stop.load(Ordering::SeqCst),
        }
    }
}

fn project_shutdown_park(observed: &[ShutdownObservation]) -> aterm_spec::interp::State {
    let at = |action: &str| observed.iter().position(|step| step.action == action);
    let (park, stop, nudge) = (at("Park"), at("Stop"), at("Nudge"));
    aterm_spec::interp::State::from([
        ("checked", i64::from(park.is_none())),
        ("parked", i64::from(park.is_some())),
        (
            "stopped",
            i64::from(observed.last().is_some_and(|step| step.stopped)),
        ),
        (
            "notified",
            i64::from(park.zip(stop).is_some_and(|(park, stop)| park < stop)),
        ),
        ("nudged", i64::from(nudge.is_some())),
        (
            "woken",
            i64::from(park.zip(nudge).is_some_and(|(park, nudge)| park < nudge)),
        ),
    ])
}

/// A schedule that probes again only an hour after an Up, and at once for a path's return
/// (no settle and no gap).
fn slow_but_nudged() -> NetState {
    NetState::with_cadence(
        BUDGET,
        netwatch::Cadence {
            steady: Duration::from_secs(3600),
            gap: Duration::ZERO,
            settle: Duration::ZERO,
        },
    )
}

/// A probe that measures Up over a loopback listener that never accepts, and says each
/// resolve on a channel.
struct Answering {
    listener: TcpListener,
    resolved: Mutex<mpsc::Sender<()>>,
}

impl Answering {
    fn new() -> (Arc<Self>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        (
            Arc::new(Self {
                listener,
                resolved: Mutex::new(tx),
            }),
            rx,
        )
    }
}

impl Probe for Answering {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        assert_eq!((host, port), (API_HOST, API_PORT));
        let _ = self.resolved.lock().unwrap().send(());
        Ok(vec![self.listener.local_addr()?])
    }
    fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream> {
        SystemProbe.connect(addr, within)
    }
    fn handshake(&self, _: TcpStream, _: &str, _: aterm_http::Deadline) -> io::Result<()> {
        Ok(())
    }
}

/// A path source that hands its sink to the test, counts the watches it started, and
/// says when the watch is dropped.
#[derive(Default)]
struct Handing {
    sink: Mutex<Option<PathSink>>,
    started: AtomicUsize,
    dropped: Arc<AtomicBool>,
}

/// [`Handing`]'s watch: its drop is the stop's witness.
struct HandedWatch(Arc<AtomicBool>);

impl Drop for HandedWatch {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl PathSource for Handing {
    fn watch(&self, sink: PathSink) -> Option<PathWatch> {
        self.started.fetch_add(1, Ordering::SeqCst);
        *self.sink.lock().unwrap() = Some(sink);
        Some(Box::new(HandedWatch(Arc::clone(&self.dropped))))
    }
}

impl Handing {
    /// The watch reads the path: `satisfied`, on the calling thread.
    fn read(&self, satisfied: bool) {
        self.send(PathEvent::Usable(satisfied));
    }

    /// The watch hears `event`, on the calling thread.
    fn send(&self, event: PathEvent) {
        let sink = self.sink.lock().unwrap();
        (sink.as_ref().expect("a watch started"))(event);
    }
}

/// A SETTLED PLACE MEASURES THE API ONCE, with no session asking: a peek starts the path's
/// watch and takes no lease, a route move then owes nothing, and the network place settling
/// onto a network probes the API (the place's internet axis then reads what this network
/// gives, not "path up, not measured").
/// NEGATIVE CONTROL: `settled` without its `ask` (a route move's handling alone) probes
/// nothing, and the wait for the probe's resolve hangs past the one-minute detector: "the
/// settled network is measured".
/// NEGATIVE CONTROL: `heard`'s Moved arm handed to `remeasure` takes the lease at once (the
/// witness is the state's lease count and the spawned thread, read on this thread, never a
/// probe's resolve that a thread may not have sent yet): "a route move owes no session-less
/// probe".
#[test]
fn a_settled_place_measures_the_api_once_with_no_session_asking() {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with(probe, Duration::from_secs(5));
    let path = Arc::new(Handing::default());
    assert!(np.path_from(Arc::clone(&path) as Arc<dyn PathSource>));
    assert_eq!(np.peek(), Reach::Unknown, "a peek takes no lease");
    path.send(PathEvent::Moved);
    assert_eq!(
        np.lock().waiting(Instant::now()),
        0,
        "a route move owes no session-less probe"
    );
    assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "nor a thread");
    path.send(PathEvent::Settled);
    assert!(
        resolved.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
        "the settled network is measured"
    );
    np.stop();
}

/// THE PATH's RETURN IS MEASURED ONCE, with no session asking: a loss and the usable reading
/// after it owe a session-less probe nothing (a read takes no lease), and the return told
/// after it ([`PathEvent::Back`]) probes the API under the place's lease: the network the
/// path came back to was not left, so no place settles, and the internet answer from before
/// the loss would stand untested.
/// NEGATIVE CONTROL: `heard`'s Back arm handed to `path_moved` (no `remeasure`) probes
/// nothing, and the wait hangs past the one-minute detector: "the path's return is measured".
#[test]
fn the_paths_return_after_a_loss_measures_the_api_once_with_no_session_asking() {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with(probe, Duration::from_secs(5));
    let path = Arc::new(Handing::default());
    assert!(np.path_from(Arc::clone(&path) as Arc<dyn PathSource>));
    assert_eq!(np.peek(), Reach::Unknown, "a peek takes no lease");
    path.read(true);
    path.read(false);
    path.read(true);
    assert_eq!(
        np.lock().waiting(Instant::now()),
        0,
        "the readings alone owe no session-less probe"
    );
    assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "nor a thread");
    path.send(PathEvent::Back);
    assert!(
        resolved.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
        "the path's return is measured"
    );
    np.stop();
}

/// THE PATH's RETURN IS A USABLE READING AFTER AN UNUSABLE ONE, once: the watch's first
/// reading is none (nothing was lost), nor a usable one after a usable one, nor a loss.
/// NEGATIVE CONTROL: `read` answering `usable` alone (no look at the last reading) fails
/// "the first reading is no return".
#[test]
fn the_paths_return_is_a_usable_reading_after_an_unusable_one() {
    let mut edge = PathEdge::default();
    assert!(!edge.read(true), "the first reading is no return");
    assert!(!edge.read(true), "nor a usable one after a usable one");
    assert!(!edge.read(false), "a loss is no return");
    assert!(!edge.read(false));
    assert!(edge.read(true), "the return after the loss");
    assert!(!edge.read(true), "once");
    let mut first_lost = PathEdge::default();
    assert!(!first_lost.read(false));
    assert!(first_lost.read(true), "a watch that began lost");
}

/// A WAKE FROM SLEEP IS MEASURED ONCE, with no session asking: on the network this Mac slept
/// on no place settles, and the answer measured before the sleep says nothing of now.
/// NEGATIVE CONTROL: `heard`'s Woke arm doing nothing probes nothing, and the wait hangs past
/// the one-minute detector: "the wake is measured".
#[test]
fn a_wake_measures_the_api_once_with_no_session_asking() {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with(probe, Duration::from_secs(5));
    let path = Arc::new(Handing::default());
    assert!(np.path_from(Arc::clone(&path) as Arc<dyn PathSource>));
    assert_eq!(np.peek(), Reach::Unknown, "a peek takes no lease");
    path.read(true);
    assert_eq!(
        np.lock().waiting(Instant::now()),
        0,
        "the first reading owes none"
    );
    assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "nor a thread");
    path.send(PathEvent::Woke);
    assert!(
        resolved.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
        "the wake is measured"
    );
    np.stop();
}

/// A WAKE IS THE FIRST `Woke` AFTER A WILL-SLEEP: `aterm_uds::power` hands an idle sleep that
/// did not come (`kIOMessageSystemWillNotSleep`, no will-sleep before it) as `Woke` too, and
/// nothing slept then; each sleep owes one measure.
/// NEGATIVE CONTROL: `hear` answering `true` for every `Woke` fails "an idle sleep that did
/// not come is no wake".
#[test]
fn a_wake_is_the_first_woke_after_a_will_sleep() {
    use aterm_uds::power::Power;
    let mut edge = WakeEdge::default();
    assert!(
        !edge.hear(Power::Woke),
        "an idle sleep that did not come is no wake"
    );
    assert!(!edge.hear(Power::WillSleep), "a will-sleep is no wake");
    assert!(edge.hear(Power::Woke), "the wake after it");
    assert!(!edge.hear(Power::Woke), "one measure a sleep");
    assert!(!edge.hear(Power::WillSleep));
    assert!(edge.hear(Power::Woke), "the next sleep's wake");
}

/// A SETTLED PLACE READING DOWN IS MEASURED AGAIN UNTIL UP: no session asks, so the settled
/// lease alone keeps the probe going, and each measure that reads down renews it within the
/// settle's watch, past one lease's length; an Up ends it, and the lease then lapses. A watch
/// that ran out renews nothing.
/// NEGATIVE CONTROL: `keep_settled` renewing nothing (its first arm returning `None` without
/// the `ask`) lets the lease lapse a minute after the settle, and "measured again while
/// down" fails.
#[test]
fn a_settled_place_reading_down_is_measured_again_until_up() {
    let (t, w) = (Instant::now(), SystemTime::now());
    let at = |now: Instant| (now, w + now.saturating_duration_since(t));
    let mut st = NetState::with_budget(Duration::from_secs(5));
    let _ = st.ask(SETTLED_LEASE, t, w);
    let mut until = Some(t + SETTLED_DOWN_FOR);
    let mut now = t;
    while now <= t + netwatch::NET_LEASE + Duration::from_secs(10) {
        let (due, wall) = at(now);
        now = st
            .next_probe(due, wall)
            .expect("measured again while down")
            .max(now);
        assert!(st.begin(now));
        let _ = st.finish(Outcome::Down, now, at(now).1);
        until = keep_settled(&mut st, Outcome::Down, until, at(now));
    }
    assert!(st.begin(now));
    let _ = st.finish(Outcome::Up, now, at(now).1);
    until = keep_settled(&mut st, Outcome::Up, until, at(now));
    assert_eq!(until, None, "an Up ends the watch");
    let lapsed = now + netwatch::NET_LEASE;
    assert_eq!(
        st.next_probe(lapsed, at(lapsed).1),
        None,
        "then the lease lapses"
    );
    assert_eq!(
        keep_settled(&mut st, Outcome::Down, Some(t), at(lapsed)),
        None,
        "a watch that ran out renews nothing"
    );
}

/// The real probe loop, parked an hour after an Up, and `event` published at its checked,
/// mutex-held park boundary by `publisher`: the shipping [`NetProbe::path_moved`], or a
/// notification ahead of the nudge it announces. The event is a path's return (the path
/// lost before the park) or a route move (the path usable throughout). Either way the worker
/// then probes it (the unlocked one only once rescued) and stops.
fn nudge_at_park_trace(publisher: ShutdownPublisher, event: PathEvent) -> Vec<ShutdownObservation> {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with_state(probe, slow_but_nudged());
    let released = Arc::downgrade(&np);
    let (t, w) = (Instant::now(), SystemTime::now());
    {
        let mut st = np.lock();
        st.ask("s-1", t, w);
        assert!(!st.path(true, t, w));
        if event == PathEvent::Usable(true) {
            assert!(
                !st.path(false, t, w),
                "PRECONDITION: a lost path owes no probe"
            );
        }
    }
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (checked_tx, checked_rx) = mpsc::channel();
    let (park_tx, park_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let np = Arc::clone(&np);
        let trace = Arc::clone(&trace);
        std::thread::spawn(move || {
            let mut first_park = true;
            np.run_before_park(|| {
                if first_park {
                    first_park = false;
                    checked_tx.send(()).unwrap();
                    park_rx.recv().unwrap();
                    trace
                        .lock()
                        .unwrap()
                        .push(ShutdownObservation::read("Park", &np));
                }
            });
            done_tx.send(()).unwrap();
        })
    };
    resolved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the lease's first probe");
    checked_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("worker reached its checked, mutex-held park boundary after the Up");
    let nudger = match publisher {
        ShutdownPublisher::HistoricalUnlocked => {
            np.wake.notify_all();
            trace
                .lock()
                .unwrap()
                .push(ShutdownObservation::read("Nudge", &np));
            None
        }
        ShutdownPublisher::MutexGuarded => {
            let np = Arc::clone(&np);
            let trace = Arc::clone(&trace);
            Some(std::thread::spawn(move || {
                np.path_moved(event);
                trace
                    .lock()
                    .unwrap()
                    .push(ShutdownObservation::read("Nudge", &np));
            }))
        }
    };
    park_tx.send(()).unwrap();
    if matches!(publisher, ShutdownPublisher::HistoricalUnlocked) {
        let owed = match event {
            PathEvent::Usable(satisfied) => {
                np.lock().path(satisfied, Instant::now(), SystemTime::now())
            }
            PathEvent::Moved | PathEvent::Settled => np.lock().moved(Instant::now()),
            PathEvent::Back | PathEvent::Woke => false,
        };
        assert!(
            owed,
            "the nudge its notification announced, published with no wake of its own"
        );
        let _state = np.lock();
        np.wake.notify_all();
    }
    if let Some(nudger) = nudger {
        nudger.join().unwrap();
    }
    resolved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the parked worker woke and probed the path's return");
    np.stop();
    done_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("stopped worker exits without another ask or probe");
    worker.join().unwrap();
    drop(np);
    assert!(released.upgrade().is_none(), "no worker retains the probe");
    Arc::try_unwrap(trace).unwrap().into_inner().unwrap()
}

/// A path's return reaches a probe thread parked for an hour: `path_moved` publishes the
/// owed probe and its wake under the state mutex, so the worker that checked its schedule
/// and parks wakes and probes the return at once (NudgedWaiterHasWake). A notification ahead
/// of the nudge, outside the mutex, is the model's `Buggy=2`, which the trace replays.
#[test]
fn a_paths_return_cannot_lose_the_park_wake_and_the_parked_probe_measures_it() {
    let model = aterm_models_gui::netprobe_shutdown_park_model();
    let observed = nudge_at_park_trace(ShutdownPublisher::MutexGuarded, PathEvent::Usable(true));
    assert_eq!(
        observed.iter().map(|step| step.action).collect::<Vec<_>>(),
        ["Park", "Nudge"]
    );
    let mut state = project_shutdown_park(&[]);
    assert_eq!(state, model.init_state());
    for (index, step) in observed.iter().enumerate() {
        assert!(model.fire(step.action, &mut state), "{step:?}: {state:?}");
        assert_eq!(state, project_shutdown_park(&observed[..=index]));
        assert!(model.check_invariant("NudgedWaiterHasWake", &state));
        assert!(model.check_invariant("StoppedWaiterHasWake", &state));
    }

    let old_order = nudge_at_park_trace(
        ShutdownPublisher::HistoricalUnlocked,
        PathEvent::Usable(true),
    );
    assert_eq!(
        old_order.iter().map(|step| step.action).collect::<Vec<_>>(),
        ["Nudge", "Park"]
    );
    assert!(!model.action_enabled(old_order[0].action, &project_shutdown_park(&[])));
    let mutant = aterm_spec::interp::with_buggy(&model, 2);
    let mut state = project_shutdown_park(&[]);
    assert_eq!(state, mutant.init_state());
    for (index, step) in old_order.iter().enumerate() {
        assert!(mutant.fire(step.action, &mut state));
        assert_eq!(state, project_shutdown_park(&old_order[..=index]));
    }
    assert!(
        !mutant.check_invariant("NudgedWaiterHasWake", &state),
        "a notification ahead of the nudge must be caught"
    );
}

/// A route move on a usable path reaches a probe thread parked for an hour the same way: it
/// publishes the owed probe and its wake under the state mutex (NudgedWaiterHasWake), so the
/// parked worker probes at once, and says so in one line; a notification ahead of it is the
/// model's `Buggy=2`.
#[test]
fn a_route_move_cannot_lose_the_park_wake_and_the_parked_probe_measures_it() {
    let model = aterm_models_gui::netprobe_shutdown_park_model();
    let observed = nudge_at_park_trace(ShutdownPublisher::MutexGuarded, PathEvent::Moved);
    assert_eq!(
        observed.iter().map(|step| step.action).collect::<Vec<_>>(),
        ["Park", "Nudge"]
    );
    let mut state = project_shutdown_park(&[]);
    for (index, step) in observed.iter().enumerate() {
        assert!(model.fire(step.action, &mut state), "{step:?}: {state:?}");
        assert_eq!(state, project_shutdown_park(&observed[..=index]));
        assert!(model.check_invariant("NudgedWaiterHasWake", &state));
    }
    let old_order = nudge_at_park_trace(ShutdownPublisher::HistoricalUnlocked, PathEvent::Moved);
    let mutant = aterm_spec::interp::with_buggy(&model, 2);
    let mut state = project_shutdown_park(&[]);
    for step in &old_order {
        assert!(mutant.fire(step.action, &mut state));
    }
    assert!(
        !mutant.check_invariant("NudgedWaiterHasWake", &state),
        "a notification ahead of the move's nudge must be caught"
    );

    let (probe, _resolved) = Answering::new();
    let np = NetProbe::with_state(probe, slow_but_nudged());
    let moved_lines = || {
        crate::test_log::logged_while(|| np.path_moved(PathEvent::Moved))
            .into_iter()
            .filter(|line| line.starts_with("harness: the network's routes moved"))
            .count()
    };
    assert_eq!(moved_lines(), 0, "no lease: nothing owed, nothing said");
    let (t, w) = (Instant::now(), SystemTime::now());
    np.lock().ask("s-1", t, w);
    assert_eq!(moved_lines(), 1, "a move that owes a probe is said once");
}

/// The installed path source starts at the probe's first ask and only then, once; its
/// readings reach the schedule (a loss reads Down at once, a return wakes the parked
/// thread to probe it) and each change is logged once; stop drops the watch, and a stopped
/// probe starts none.
#[test]
fn the_first_ask_starts_the_path_watch_once_and_stop_drops_it() {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with_state(probe, slow_but_nudged());
    np.watch_path();
    let source = Arc::new(Handing::default());
    assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
    assert!(
        !np.path_from(Arc::new(Handing::default())),
        "one source, installed once"
    );
    assert_eq!(
        source.started.load(Ordering::SeqCst),
        0,
        "not before an ask"
    );
    assert_eq!(np.ask("s-1"), Reach::Unknown);
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "the first ask");
    resolved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the first probe");
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !matches!(np.ask("s-1"), Reach::Up { .. }) {
        assert!(Instant::now() < until, "the first probe's Up never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "only the first");
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&lines);
    crate::test_log::observe(
        move |record| {
            let line = record.args().to_string();
            if line.starts_with("harness: network path") {
                seen.lock().unwrap().push(line);
            }
        },
        || {
            source.read(true);
            source.read(false);
            assert!(
                matches!(
                    np.lock().reach(Instant::now(), SystemTime::now()),
                    Reach::Down { .. }
                ),
                "a lost path reads Down at once"
            );
            source.read(false);
            source.read(true);
        },
    );
    resolved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the return woke the parked thread to probe it");
    let lines = lines.lock().unwrap();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("network path lost"), "{lines:?}");
    assert!(lines[1].contains("network path back"), "{lines:?}");
    assert!(!source.dropped.load(Ordering::SeqCst));
    np.stop();
    assert!(
        source.dropped.load(Ordering::SeqCst),
        "stop drops the watch"
    );
    np.watch_path();
    assert_eq!(
        source.started.load(Ordering::SeqCst),
        1,
        "a stopped probe starts no watch"
    );
}

/// A session the probe does not measure reads the path alone: its first read starts the
/// installed watch (once ever, shared with a later ask), and no read takes a lease, starts
/// the thread or probes the API host. A loss reads Down at once with a look owed, and the
/// return (no settle here) Unknown with none.
#[test]
fn a_path_only_read_starts_the_watch_once_and_probes_nothing() {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with_state(probe, slow_but_nudged());
    assert_eq!(np.path_reach(), Reach::Unknown, "no source: no reading");
    let source = Arc::new(Handing::default());
    assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
    assert_eq!(np.path_reach(), Reach::Unknown, "no reading yet");
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "the first read");
    assert_eq!(np.path_reach(), Reach::Unknown);
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "only the first");
    assert_eq!(np.path_landing(), None, "nothing lost");
    source.read(true);
    source.read(false);
    let read = Instant::now();
    let Reach::Down { since } = np.path_reach() else {
        panic!("a lost path reads Down at once: {:?}", np.path_reach());
    };
    assert!(since <= read, "since the loss");
    assert!(
        np.path_landing().is_some_and(|at| at > read),
        "a lost path owes a look"
    );
    source.read(true);
    assert_eq!(np.path_reach(), Reach::Unknown, "the return, settled");
    assert_eq!(np.path_landing(), None);
    assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "no thread");
    assert!(!np.started.load(Ordering::SeqCst));
    assert_eq!(np.lock().waiting(Instant::now()), 0, "no lease");
    assert!(resolved.try_recv().is_err(), "nothing probed");
    assert_eq!(
        np.ask("s-1"),
        Reach::Down { since },
        "a measured session's return stands Down until a probe measures it"
    );
    assert_eq!(
        source.started.load(Ordering::SeqCst),
        1,
        "an ask after a path-only read starts no second watch"
    );
    resolved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the ask's probe");
    np.stop();
    assert!(source.dropped.load(Ordering::SeqCst), "stop drops it");
    assert_eq!(np.path_reach(), Reach::Unknown, "the last reading stands");
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "none after stop");
}

/// A peek reads the verdict a measured session's ask would get, unasked: its first read
/// starts the installed path watch (once ever), and no peek takes a lease, starts the thread
/// or probes the API host. A path that can carry traffic reads no Up; a lost path reads Down
/// at once, and so does its return until a probe measures it; a measure reads as measured.
/// NEGATIVE CONTROL (run): with [`NetProbe::peek`]'s `watch_path` call removed, the first
/// peek starts no watch and the test fails at "the first peek".
#[test]
fn a_peek_reads_the_measured_verdict_with_no_lease_thread_or_probe() {
    let (probe, resolved) = Answering::new();
    let np = NetProbe::with_state(probe, slow_but_nudged());
    assert_eq!(np.peek(), Reach::Unknown, "no source: nothing to read");
    let source = Arc::new(Handing::default());
    assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
    assert_eq!(np.peek(), Reach::Unknown, "nothing read or measured yet");
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "the first peek");
    source.read(true);
    assert_eq!(np.peek(), Reach::Unknown, "a working path is no Up");
    source.read(false);
    let read = Instant::now();
    let Reach::Down { since } = np.peek() else {
        panic!("a lost path reads Down at once: {:?}", np.peek());
    };
    assert!(since <= read, "since the loss");
    source.read(true);
    assert_eq!(
        np.peek(),
        Reach::Down { since },
        "the return stands Down until a probe measures it"
    );
    let measured = Instant::now();
    np.settle(Outcome::Up, measured, SystemTime::now());
    assert!(
        matches!(np.peek(), Reach::Up { since } if since == measured),
        "the measure reads as measured: {:?}",
        np.peek()
    );
    assert_eq!(source.started.load(Ordering::SeqCst), 1, "only the first");
    assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "no thread");
    assert!(!np.started.load(Ordering::SeqCst));
    assert_eq!(np.lock().waiting(Instant::now()), 0, "no lease");
    assert!(resolved.try_recv().is_err(), "nothing probed");
    np.stop();
}

/// Network.framework's satisfiable path (a connection attempt brings it up, as an on-demand
/// VPN) reads usable, as a satisfied one does; an unsatisfied one does not, and an invalid
/// one says nothing. Only the route socket's moves are passed on.
#[test]
fn a_satisfiable_path_reads_usable_and_only_route_moves_pass() {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::path_monitor::PathStatus;
        assert_eq!(usable(PathStatus::Satisfied), Some(true));
        assert_eq!(
            usable(PathStatus::Satisfiable),
            Some(true),
            "a satisfiable path is one a connection brings up"
        );
        assert_eq!(usable(PathStatus::Unsatisfied), Some(false));
        assert_eq!(usable(PathStatus::Invalid), None);
    }
    use crate::network_moved::Woke;
    assert!(route_move(Woke::Route));
    assert!(!route_move(Woke::Browse));
    for other in [Woke::Timer, Woke::Beacon] {
        assert!(!route_move(other), "{other:?}");
    }
}

/// [`RouteMoves`] passes each route move its watch hears on as one [`PathEvent::Moved`], and
/// once dropped its thread ends at the next wake, passing nothing more: its sink, which only
/// the thread holds, is released.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn route_moves_pass_each_route_move_and_end_once_dropped() {
    use crate::network_moved::{Moved, Woke};
    let moved = Arc::new(Moved::new());
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    let sink: Arc<dyn Fn(PathEvent) + Send + Sync> =
        Arc::new(move |event| seen.lock().unwrap().push(event));
    let held = Arc::downgrade(&sink);
    let routes = RouteMoves::start(sink, moved.watch()).expect("the thread starts");
    let until = |count: usize, what: &str| {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while events.lock().unwrap().len() < count {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    moved.tell(Woke::Route);
    until(1, "the route move passed on");
    moved.tell(Woke::Route);
    until(2, "the next one too");
    drop(routes);
    moved.tell(Woke::Route);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while held.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "the dropped watch's thread never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        *events.lock().unwrap(),
        [PathEvent::Moved, PathEvent::Moved],
        "nothing passed after the drop"
    );
}

/// A path source whose watch cannot start is tried once, not at every read.
#[test]
fn a_watch_that_cannot_start_is_tried_once() {
    #[derive(Default)]
    struct Refusing(AtomicUsize);
    impl PathSource for Refusing {
        fn watch(&self, _: PathSink) -> Option<PathWatch> {
            self.0.fetch_add(1, Ordering::SeqCst);
            None
        }
    }
    let (probe, _resolved) = Answering::new();
    let np = NetProbe::with_state(probe, slow_but_nudged());
    let source = Arc::new(Refusing::default());
    assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
    for _ in 0..3 {
        assert_eq!(np.path_reach(), Reach::Unknown);
    }
    np.watch_path();
    assert_eq!(source.0.load(Ordering::SeqCst), 1);
}

fn shutdown_at_park_trace(publisher: ShutdownPublisher) -> Vec<ShutdownObservation> {
    let probe = Arc::new(Seam::new(
        Err(io::Error::other("this fixture must never resolve")),
        Dial::Unreachable,
        Dial::Unreachable,
    ));
    let np = NetProbe::with(probe, BUDGET);
    let released = Arc::downgrade(&np);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (checked_tx, checked_rx) = mpsc::channel();
    let (park_tx, park_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = {
        let np = Arc::clone(&np);
        let trace = Arc::clone(&trace);
        std::thread::spawn(move || {
            let mut first_park = true;
            np.run_before_park(|| {
                if first_park {
                    first_park = false;
                    checked_tx.send(()).unwrap();
                    park_rx.recv().unwrap();
                    trace
                        .lock()
                        .unwrap()
                        .push(ShutdownObservation::read("Park", &np));
                }
            });
            done_tx.send(()).unwrap();
        })
    };
    checked_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("worker reached its checked, mutex-held park boundary");
    let stopper = match publisher {
        ShutdownPublisher::HistoricalUnlocked => {
            np.stop.store(true, Ordering::SeqCst);
            np.wake.notify_all();
            trace
                .lock()
                .unwrap()
                .push(ShutdownObservation::read("Stop", &np));
            None
        }
        ShutdownPublisher::MutexGuarded => {
            let np = Arc::clone(&np);
            let trace = Arc::clone(&trace);
            Some(std::thread::spawn(move || {
                np.stop();
                trace
                    .lock()
                    .unwrap()
                    .push(ShutdownObservation::read("Stop", &np));
            }))
        }
    };
    park_tx.send(()).unwrap();
    if matches!(publisher, ShutdownPublisher::HistoricalUnlocked) {
        let _state = np.lock();
        np.wake.notify_all();
    }
    if let Some(stopper) = stopper {
        stopper.join().unwrap();
    }
    done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("stopped worker exits without another ask or probe");
    worker.join().unwrap();
    drop(np);
    assert!(released.upgrade().is_none(), "no worker retains the probe");
    Arc::try_unwrap(trace).unwrap().into_inner().unwrap()
}

#[test]
fn shutdown_cannot_lose_the_park_wake_or_retain_the_worker() {
    let model = aterm_models_gui::netprobe_shutdown_park_model();
    let observed = shutdown_at_park_trace(ShutdownPublisher::MutexGuarded);
    assert_eq!(
        observed.iter().map(|step| step.action).collect::<Vec<_>>(),
        ["Park", "Stop"]
    );
    let mut state = project_shutdown_park(&[]);
    assert_eq!(state, model.init_state());
    for (index, step) in observed.iter().enumerate() {
        assert!(model.fire(step.action, &mut state), "{step:?}: {state:?}");
        assert_eq!(state, project_shutdown_park(&observed[..=index]));
        assert!(model.check_invariant("StoppedWaiterHasWake", &state));
    }

    let old_order = shutdown_at_park_trace(ShutdownPublisher::HistoricalUnlocked);
    assert_eq!(
        old_order.iter().map(|step| step.action).collect::<Vec<_>>(),
        ["Stop", "Park"]
    );
    assert!(!model.action_enabled(old_order[0].action, &project_shutdown_park(&[])));
    let mutant = aterm_spec::interp::with_buggy(&model, 1);
    let mut state = project_shutdown_park(&[]);
    assert_eq!(state, mutant.init_state());
    for (index, step) in old_order.iter().enumerate() {
        assert!(mutant.fire(step.action, &mut state));
        assert_eq!(state, project_shutdown_park(&old_order[..=index]));
    }
    assert!(
        !mutant.check_invariant("StoppedWaiterHasWake", &state),
        "the old notification before parking must be caught"
    );
}

/// The probe thread's settle logs one line per change of the measured verdict, on the
/// thread that settled, and none for a repeat: Down, Down, Up, Up is two lines, the
/// second the Down→Up edge with the lease that was waiting.
#[test]
fn each_change_of_the_measured_reach_is_logged_once_and_a_repeat_is_not() {
    const S: Duration = Duration::from_secs(1);
    let probe = Arc::new(Seam::new(
        Err(io::Error::other("this fixture must never resolve")),
        Dial::Unreachable,
        Dial::Unreachable,
    ));
    let np = NetProbe::with(probe, BUDGET);
    let (t, w) = (Instant::now(), SystemTime::now());
    assert_eq!(np.lock().ask("s-1", t, w), Reach::Unknown);
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&lines);
    crate::test_log::observe(
        move |record| {
            let line = record.args().to_string();
            if line.starts_with("harness: API reach") {
                seen.lock().unwrap().push(line);
            }
        },
        || {
            for (outcome, at) in [
                (Outcome::Down, 0),
                (Outcome::Down, 3),
                (Outcome::Up, 18),
                (Outcome::Up, 50),
            ] {
                let began = t + at * S;
                assert!(np.lock().begin(began));
                np.settle(outcome, began + S / 10, w + at * S);
            }
        },
    );
    let lines = lines.lock().unwrap();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains("reach down (no fresh measure"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("down→up after 18s") && lines[1].contains("1 waiting"),
        "{lines:?}"
    );
    assert_eq!(
        np.lock().reach(t + 51 * S, w + 51 * S),
        Reach::Up {
            since: t + 18 * S + S / 10
        }
    );
}

fn refused_port() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
        assert!(
            Instant::now() < deadline,
            "{addr} still accepts a minute after its listener was dropped"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    addr
}

fn v6(n: u16) -> SocketAddr {
    SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, n), 443))
}

/// What a fake connect to one address does.
#[derive(Clone, Copy, Debug)]
enum Dial {
    /// The kernel's own `EHOSTUNREACH`.
    Unreachable,
    /// A black hole: the whole share spent, then a timeout.
    BlackHole,
    /// The real connector (a loopback address).
    Real,
}

struct Seam {
    addrs: io::Result<Vec<SocketAddr>>,
    v6: Dial,
    v4: Dial,
    handshake: Option<bool>,
    dialled: Mutex<Vec<(SocketAddr, Duration)>>,
}

impl Seam {
    fn new(addrs: io::Result<Vec<SocketAddr>>, v6: Dial, v4: Dial) -> Self {
        Self {
            addrs,
            v6,
            v4,
            handshake: None,
            dialled: Mutex::default(),
        }
    }
}

impl Probe for Seam {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        assert_eq!((host, port), (API_HOST, API_PORT));
        match &self.addrs {
            Ok(a) => Ok(a.clone()),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        }
    }
    fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream> {
        self.dialled.lock().unwrap().push((addr, within));
        match if addr.is_ipv4() { self.v4 } else { self.v6 } {
            #[cfg(unix)]
            Dial::Unreachable => Err(io::Error::from_raw_os_error(libc::EHOSTUNREACH)),
            #[cfg(not(unix))]
            Dial::Unreachable => Err(io::Error::from(io::ErrorKind::HostUnreachable)),
            Dial::BlackHole => {
                std::thread::sleep(within);
                Err(io::Error::new(io::ErrorKind::TimedOut, "black hole"))
            }
            Dial::Real => SystemProbe.connect(addr, within),
        }
    }
    fn handshake(
        &self,
        tcp: TcpStream,
        host: &str,
        deadline: aterm_http::Deadline,
    ) -> io::Result<()> {
        match self.handshake {
            Some(true) => Ok(()),
            Some(false) => Err(io::Error::other("TLS handshake failed: bad certificate")),
            None => verified_handshake(tcp, host, deadline),
        }
    }
}

fn answering(reply: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            use std::io::{Read as _, Write as _};
            let mut hello = [0u8; 512];
            let _ = s.read(&mut hello);
            let _ = s.write_all(reply);
        }
    });
    addr
}

#[test]
fn the_probe_reads_down_only_on_definite_failures() {
    let nx = Seam::new(
        Err(io::Error::from(io::ErrorKind::NotFound)),
        Dial::Real,
        Dial::Real,
    );
    assert_eq!(probe_once(&nx, BUDGET), Outcome::Down, "NXDOMAIN");
    #[cfg(target_os = "macos")]
    {
        let macos_no_name = Seam::new(
            Err(io::Error::other(
                "failed to lookup address information: nodename nor servname provided, or not known",
            )),
            Dial::Real,
            Dial::Real,
        );
        assert_eq!(probe_once(&macos_no_name, BUDGET), Outcome::Down);
    }
    let temporary = Seam::new(
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "temporary DNS failure",
        )),
        Dial::Real,
        Dial::Real,
    );
    assert_eq!(
        probe_once(&temporary, BUDGET),
        Outcome::Unknown,
        "a temporary resolver error keeps the normal retry ladder"
    );
    let unrecognized = Seam::new(
        Err(io::Error::other(
            "failed to lookup address information: unexpected error",
        )),
        Dial::Real,
        Dial::Real,
    );
    assert_eq!(probe_once(&unrecognized, BUDGET), Outcome::Unknown);
    let refused = Seam::new(Ok(vec![refused_port()]), Dial::Real, Dial::Real);
    assert_eq!(probe_once(&refused, BUDGET), Outcome::Down, "refused");
    let both = Seam::new(
        Ok(vec![v6(1), refused_port()]),
        Dial::Unreachable,
        Dial::Real,
    );
    assert_eq!(
        probe_once(&both, BUDGET),
        Outcome::Down,
        "unreachable v6, refused v4"
    );
    let hole = Seam::new(Ok(vec![v6(1), refused_port()]), Dial::BlackHole, Dial::Real);
    assert_eq!(
        probe_once(&hole, Duration::from_millis(200)),
        Outcome::Unknown
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let open = listener.local_addr().expect("addr");
    let mut portal = Seam::new(Ok(vec![open]), Dial::Real, Dial::Real);
    portal.handshake = Some(false);
    assert_eq!(probe_once(&portal, BUDGET), Outcome::Unknown);
    let mut good = Seam::new(Ok(vec![open]), Dial::Real, Dial::Real);
    good.handshake = Some(true);
    assert_eq!(probe_once(&good, BUDGET), Outcome::Up);
}

#[test]
fn a_black_holed_ipv6_cannot_eat_the_budget() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let v4 = listener.local_addr().expect("addr");
    let mut seam = Seam::new(Ok(vec![v6(1), v6(2), v4]), Dial::BlackHole, Dial::Real);
    seam.handshake = Some(true);
    assert_eq!(probe_once(&seam, BUDGET), Outcome::Up);
    let dialled = seam.dialled.lock().unwrap();
    assert_eq!(dialled[0].0, v4, "IPv4 first: {dialled:?}");
    assert!(dialled[0].1 <= BUDGET / 3 + Duration::from_millis(5));
    assert_eq!(dialled.len(), 1, "{dialled:?}");
    drop(listener);
}

#[test]
fn a_garbage_answer_to_the_client_hello_reads_unknown() {
    let addr = answering(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
    let seam = Seam::new(Ok(vec![addr]), Dial::Real, Dial::Real);
    assert_eq!(probe_once(&seam, BUDGET), Outcome::Unknown);
}

#[test]
fn a_resolver_that_blocks_past_the_budget_reads_unknown_and_starts_no_second_probe() {
    struct Stuck {
        gate: Mutex<mpsc::Receiver<()>>,
        calls: AtomicUsize,
    }
    impl Probe for Stuck {
        fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _ = self.gate.lock().unwrap().recv();
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
        fn connect(&self, _: SocketAddr, _: Duration) -> io::Result<TcpStream> {
            unreachable!("never resolved")
        }
        fn handshake(&self, _: TcpStream, _: &str, _: aterm_http::Deadline) -> io::Result<()> {
            unreachable!("never connected")
        }
    }
    let (release, gate) = mpsc::channel();
    let stuck = Arc::new(Stuck {
        gate: Mutex::new(gate),
        calls: AtomicUsize::new(0),
    });
    let np = NetProbe::with(
        Arc::clone(&stuck) as Arc<dyn Probe>,
        Duration::from_millis(40),
    );
    assert_eq!(np.ask("s-1"), Reach::Unknown, "no measure yet, no wait");
    let until = Instant::now() + Duration::from_secs(60);
    while stuck.calls.load(Ordering::SeqCst) == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..5 {
        assert_eq!(np.ask("s-1"), Reach::Unknown);
        assert_eq!(np.ask("s-2"), Reach::Unknown);
    }
    assert_eq!(stuck.calls.load(Ordering::SeqCst), 1, "one probe in flight");
    assert_eq!(np.spawned.load(Ordering::SeqCst), 1, "one thread");
    np.stop();
    drop(release);

    struct Nx;
    impl Probe for Nx {
        fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
        fn connect(&self, _: SocketAddr, _: Duration) -> io::Result<TcpStream> {
            unreachable!()
        }
        fn handshake(&self, _: TcpStream, _: &str, _: aterm_http::Deadline) -> io::Result<()> {
            unreachable!()
        }
    }
    assert_eq!(probe_once(&Nx, Duration::ZERO), Outcome::Unknown);
    let np = NetProbe::with(Arc::new(Nx), BUDGET);
    let asked = Instant::now();
    let mut seen = np.ask("s-1");
    while !matches!(seen, Reach::Down { .. }) && asked.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(5));
        seen = np.ask("s-1");
    }
    assert!(matches!(seen, Reach::Down { .. }), "{seen:?}");
    assert_eq!(np.ask("s-1"), seen);
    np.stop();
}

const MDM_DOMAIN: &str = "com.anthropic.claudecode.plist";

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-netprobe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["home/.claude", "proj/.claude", "managed", "prefs"] {
        std::fs::create_dir_all(dir.join(sub)).expect("scratch");
    }
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
}

fn homed(home: &Path) -> Vec<String> {
    let mut vars = vec![
        format!("HOME={}", home.display()),
        format!(
            "{}={}",
            aterm_agent::harness::accounts::HOMEDIR_ENV,
            home.display()
        ),
    ];
    vars.dedup();
    vars
}

#[test]
fn an_agents_settings_and_profile_store_can_take_its_stored_login_away() {
    let dir = scratch("stored");
    let home = dir.join("home");
    let env = |more: &[String]| {
        let mut env = homed(&home);
        env.push("USER=owner".to_string());
        env.extend(more.iter().cloned());
        Some(env)
    };
    let stored = |env: Option<Vec<String>>, argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        route_of_facts(
            &gather(
                env,
                &argv,
                &dir.join("proj"),
                Some(&dir.join("managed")),
                &dir.join("prefs"),
                Some(&dir.join("home")),
            ),
            Some(&home),
        )
        .is_stored()
    };
    let user = home.join(".claude/settings.json");
    assert!(stored(env(&[]), &["claude"]), "a quiet tree");
    write(&user, "{\"model\":\"opus\",\"env\":{\"EDITOR\":\"vi\"}}");
    assert!(stored(env(&[]), &["claude"]), "a model and an editor");

    for (why, text) in [
        (
            "an API key in an env block",
            "{\"env\":{\"ANTHROPIC_API_KEY\":\"k\"}}",
        ),
        ("an apiKeyHelper", "{\"apiKeyHelper\":\"/opt/key.sh\"}"),
        ("a profile", "{\"env\":{\"ANTHROPIC_PROFILE\":\"p\"}}"),
        ("a file that does not parse", "{"),
    ] {
        write(&user, text);
        assert!(!stored(env(&[]), &["claude"]), "{why}: the user's");
        write(&user, "{}");
        write(&dir.join("proj/.claude/settings.local.json"), text);
        assert!(!stored(env(&[]), &["claude"]), "{why}: the project's");
        std::fs::remove_file(dir.join("proj/.claude/settings.local.json")).expect("rm");
        assert!(
            !stored(env(&[]), &["claude", "--settings", text]),
            "{why}: --settings"
        );
        assert!(stored(env(&[]), &["claude"]), "{why}: gone again");
    }

    let configs = home.join(".config/anthropic/configs");
    std::fs::create_dir_all(&configs).expect("configs");
    write(&configs.join("notes.txt"), "x");
    assert!(stored(env(&[]), &["claude"]), "no config in the store");
    write(&configs.join("default.json"), "{}");
    assert!(!stored(env(&[]), &["claude"]), "a config in the store");
    std::fs::remove_file(configs.join("default.json")).expect("rm");
    let elsewhere = dir.join("store/configs");
    std::fs::create_dir_all(&elsewhere).expect("configs");
    write(&elsewhere.join("work.json"), "{}");
    let named = [format!(
        "ANTHROPIC_CONFIG_DIR={}",
        dir.join("store").display()
    )];
    assert!(
        !stored(env(&named), &["claude"]),
        "where the variable names it"
    );
    let cloud = [format!(
        "ANTHROPIC_CONFIG_DIR={}",
        home.join("Library/CloudStorage/x").display()
    )];
    assert!(!stored(env(&cloud), &["claude"]), "a store nobody lists");
    assert!(!home.join("Library/CloudStorage").exists(), "nor made");

    assert!(!stored(None, &["claude"]));
    let empty = RouteFacts {
        env: Some(Vec::new()),
        settings: Vec::new(),
    };
    assert!(!route_of_facts(&empty, Some(&home)).is_stored());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_route_reads_every_settings_source_claude_code_reads() {
    let proxy = "{\"env\":{\"HTTPS_PROXY\":\"http://proxy.corp:3128\"}}";
    let dir = scratch("route");
    let env = || {
        let mut env = homed(&dir.join("home"));
        env.push("USER=owner".to_string());
        Some(env)
    };
    let facts = |argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        netwatch::route_of(&gather(
            env(),
            &argv,
            &dir.join("proj"),
            Some(&dir.join("managed")),
            &dir.join("prefs"),
            Some(&dir.join("home")),
        ))
    };
    write(
        &dir.join("home/.claude/settings.json"),
        "{\"model\":\"opus\"}",
    );
    assert_eq!(facts(&["claude"]), Route::Default);
    for file in [
        "home/.claude/settings.json",
        "proj/.claude/settings.json",
        "proj/.claude/settings.local.json",
        "managed/managed-settings.json",
    ] {
        let path = dir.join(file);
        let before = std::fs::read_to_string(&path).ok();
        write(&path, proxy);
        assert!(matches!(facts(&["claude"]), Route::Custom(_)), "{file}");
        write(&path, "{\"env\":");
        assert!(
            matches!(facts(&["claude"]), Route::Custom(_)),
            "{file} broken"
        );
        match before {
            Some(text) => write(&path, &text),
            None => std::fs::remove_file(&path).expect("rm"),
        }
        assert_eq!(facts(&["claude"]), Route::Default, "{file} restored");
    }
    std::fs::create_dir_all(dir.join("managed/managed-settings.d")).expect("drop-ins");
    write(&dir.join("managed/managed-settings.d/10-ok.json"), "{}");
    assert_eq!(facts(&["claude"]), Route::Default);
    write(&dir.join("managed/managed-settings.d/20-gw.json"), proxy);
    assert!(matches!(facts(&["claude"]), Route::Custom(_)));
    std::fs::remove_file(dir.join("managed/managed-settings.d/20-gw.json")).expect("rm");
    std::fs::create_dir_all(dir.join("prefs/owner")).expect("prefs");
    write(&dir.join("prefs/owner").join(MDM_DOMAIN), "plist");
    assert!(
        matches!(facts(&["claude"]), Route::Custom(_)),
        "an MDM domain"
    );
    std::fs::remove_file(dir.join("prefs/owner").join(MDM_DOMAIN)).expect("rm");
    write(&dir.join("proj/team.json"), proxy);
    assert!(matches!(
        facts(&["claude", "--settings", "team.json"]),
        Route::Custom(_)
    ));
    assert!(matches!(
        facts(&["claude", &format!("--settings={proxy}")]),
        Route::Custom(_)
    ));
    assert_eq!(facts(&["claude", "--settings", "{}"]), Route::Default);
    for cwd in ["", "proj"] {
        let blind = netwatch::route_of(&gather(
            env(),
            &[],
            Path::new(cwd),
            Some(&dir.join("managed")),
            &dir.join("prefs"),
            Some(&dir.join("home")),
        ));
        assert!(matches!(blind, Route::Custom(_)), "{cwd:?}: {blind:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_project_in_a_file_provider_domain_is_measured_unless_a_document_is_there() {
    let dir = scratch("provider");
    let home = dir.join("home");
    let cloud = home.join("Library/CloudStorage/SyncProvider-acct/proj");
    std::fs::create_dir_all(&cloud).expect("the project");
    let facts = |cwd: &Path| {
        gather(
            Some(
                homed(&home)
                    .into_iter()
                    .chain(["USER=owner".to_string()])
                    .collect(),
            ),
            &["claude".to_string()],
            cwd,
            Some(&dir.join("managed")),
            &dir.join("prefs"),
            Some(&home),
        )
    };
    assert_eq!(netwatch::route_of(&facts(&cloud)), Route::Default);
    assert!(route_of_facts(&facts(&cloud), Some(&home)).is_stored());
    let quiet = "{\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}";
    std::fs::create_dir_all(cloud.join(".claude")).expect(".claude");
    write(&cloud.join(".claude/settings.local.json"), quiet);
    let r = netwatch::route_of(&facts(&cloud));
    assert!(
        matches!(&r, Route::Custom(why) if why.contains("settings.local.json could not be read")),
        "{r:?}"
    );
    assert!(!route_of_facts(&facts(&cloud), Some(&home)).is_stored());
    write(&dir.join("proj/.claude/settings.local.json"), quiet);
    assert_eq!(
        netwatch::route_of(&facts(&dir.join("proj"))),
        Route::Default
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A session's plain-HTTP endpoint (v6 §14.17.5.4: the lab stub's, a gateway's `http://`
/// base URL) is Up only once its server answers HTTP, whatever the status; a server that
/// accepts and says nothing, or answers no HTTP, is Unknown; a refused connection is Down.
/// NEGATIVE CONTROL (run): with `probe_target` handshaking every endpoint, as main's probe
/// did its one, the answering server reads Unknown and the test fails.
#[test]
fn a_plain_endpoint_is_up_only_once_its_server_answers_http() {
    let plain = |addr: SocketAddr| Target {
        host: addr.ip().to_string(),
        port: addr.port(),
        tls: false,
    };
    let answered = answering(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(
        probe_target(&SystemProbe, &plain(answered), BUDGET),
        Outcome::Up
    );
    for silent in [answering(b""), answering(b"SSH-2.0-OpenSSH_9.9\r\n")] {
        assert_eq!(
            probe_target(&SystemProbe, &plain(silent), BUDGET),
            Outcome::Unknown
        );
    }
    let refused = {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        listener.local_addr().expect("addr")
    };
    assert_eq!(
        probe_target(&SystemProbe, &plain(refused), BUDGET),
        Outcome::Down
    );
}

/// THE PROBE OF A SESSION'S OWN ENDPOINT (v6 §14.17.5.4): the instance's probe makes one per
/// endpoint at its first ask and hands it back after, itself for Claude Code's default route;
/// it starts the path's watch, takes every later path reading and the last one before it was
/// made, and stops with the instance's probe.
#[test]
fn a_sessions_endpoint_gets_its_own_probe_fed_the_path_and_stopped_with_it() {
    let (probe, _resolved) = Answering::new();
    let root = NetProbe::with(probe, BUDGET);
    let source = Arc::new(Handing::default());
    assert!(root.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
    let stub = Target {
        host: "127.0.0.1".to_string(),
        port: 4100,
        tls: false,
    };
    assert!(Arc::ptr_eq(&root.to(&Target::anthropic()).unwrap(), &root));
    assert_eq!(
        source.started.load(Ordering::SeqCst),
        0,
        "no watch for its own"
    );
    let child = root.to(&stub).unwrap();
    assert_eq!(child.target(), &stub);
    assert!(
        Arc::ptr_eq(&root.to(&stub).unwrap(), &child),
        "one probe per endpoint"
    );
    assert_eq!(
        source.started.load(Ordering::SeqCst),
        1,
        "the path is watched"
    );
    source.read(true);
    source.read(false);
    assert!(
        matches!(child.path_reach(), Reach::Down { .. }),
        "the path's loss reaches the endpoint's probe"
    );
    let later = Target {
        port: 4200,
        ..stub.clone()
    };
    assert!(
        matches!(root.to(&later).unwrap().path_reach(), Reach::Down { .. }),
        "a probe made after the loss starts from it"
    );
    root.stop();
    assert!(
        child.stop.load(Ordering::SeqCst),
        "stopped with the instance's probe"
    );
    assert!(
        root.to(&Target { port: 4300, ..stub })
            .unwrap()
            .stop
            .load(Ordering::SeqCst),
        "a probe made after the stop is stopped"
    );
}

/// Endpoint churn cannot leave an unbounded table or parked-thread population. This pure
/// control creates no probe lease/thread/IO, preserves existing entries at capacity, and
/// proves that an unsupported new endpoint gets no other endpoint's reading.
#[test]
fn endpoint_probe_capacity_refuses_new_targets_without_evicting_existing_ones() {
    let (probe, _resolved) = Answering::new();
    let root = NetProbe::with(probe, BUDGET);
    let target = |port| Target {
        host: "127.0.0.1".to_string(),
        port,
        tls: false,
    };
    let first = root.to(&target(1000)).unwrap();
    for n in 1..MAX_ENDPOINT_PROBES - 1 {
        assert!(root.to(&target(1000 + u16::try_from(n).unwrap())).is_some());
    }
    assert_eq!(root.others().len(), MAX_ENDPOINT_PROBES - 1);
    assert!(root.to(&target(9000)).is_none());
    assert!(Arc::ptr_eq(&root.to(&target(1000)).unwrap(), &first));
    assert!(Arc::ptr_eq(&root.to(&Target::anthropic()).unwrap(), &root));
    assert_eq!(root.others().len(), MAX_ENDPOINT_PROBES - 1);
    assert_eq!(root.spawned.load(Ordering::SeqCst), 0);
    assert!(
        root.others
            .lock()
            .unwrap()
            .values()
            .all(|p| p.spawned.load(Ordering::SeqCst) == 0)
    );
    root.stop();
    assert!(first.stop.load(Ordering::SeqCst));
}
