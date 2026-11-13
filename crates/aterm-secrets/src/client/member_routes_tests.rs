// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::AtomicUsize;

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// How long the worker runs while its waits end early: longer than one look.
const WINDOW: Duration = Duration::from_millis(1500);

/// Run the worker over `changed`, under a private broker socket, until
/// `enough(observed, waits, ran)` holds ([`HANG`] detects one that never does): how many
/// times it observed, how many times it waited, and how long it ran. `changed` hears each
/// wait's deadline and how many times the worker had observed when it began. With
/// `arrives`, a broker answers at that socket from the start (macOS, where the worker watches
/// for one); without, none ever comes.
fn observes_over(
    arrives: bool,
    mut changed: impl FnMut(Option<Instant>, usize) -> bool + Send,
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
    let changed = |deadline: Option<Instant>| {
        waits.fetch_add(1, Ordering::SeqCst);
        changed(deadline, observed.load(Ordering::SeqCst))
    };
    let began = Instant::now();
    let hear = || {
        while_hearing(
            &paths,
            &members,
            &|_: &Paths| Ok(0),
            &|_| {},
            &project,
            Rings::default(),
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

/// A wait that ends early with nothing heard (0.159.0 and 0.160.0's ended at each routing
/// message, a millisecond in) runs no round, and a worker owed nothing looks once, at the
/// look its start owes ([`Listener::new`]): the worker runs past [`WINDOW`] and through a
/// hundred waits, however slowly the machine runs. A wait that runs to its deadline looks
/// once, and through the whole wait after that look the worker looks no more.
/// NEGATIVE CONTROLS: a round that looks at every wake looks at each early one; the old
/// pace, a look a [`LOOK`] after each, looks again as the first wait after a look ends.
#[test]
fn early_wakes_with_nothing_owed_run_no_round() {
    let (observed, waits, ran) = observes_over(
        false,
        |_, _| {
            std::thread::sleep(Duration::from_millis(1));
            false
        },
        |_, waits, ran| ran >= WINDOW && waits > 100,
    );
    assert!(
        observed <= 1,
        "{observed} looks in {ran:?} from {waits} early wakes"
    );
    // The waits that began after the look: the second begins once the round after the first
    // has decided, so a look that round owed is counted.
    let after_look = AtomicUsize::new(0);
    let (observed, _, _) = observes_over(
        false,
        |deadline, observed| {
            if observed > 0 {
                after_look.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(rest(deadline));
            false
        },
        |_, _, _| after_look.load(Ordering::SeqCst) >= 2,
    );
    assert_eq!(observed, 1, "a look owed nothing after it");
}

/// One wait of the worker, as [`Hears`] tells it: its deadline, and how many looks the worker
/// had taken when it began.
type Wait = (Option<Instant>, usize);

/// A wait that hears what the worker hands it, as [`NativeChanges`]' does, with no timeout
/// past the worker's deadline (the stop's bell, always handed, ends it), and tells each wait
/// as it begins; it never hears a change of network.
struct Hears {
    waits: std::sync::mpsc::Sender<Wait>,
    looks: Arc<AtomicUsize>,
}

impl Changes for Hears {
    fn wait(&mut self, deadline: Option<Instant>, also: &[BorrowedFd<'_>]) -> bool {
        let _ = self
            .waits
            .send((deadline, self.looks.load(Ordering::SeqCst)));
        let _ = aterm_uds::ready::any(also, deadline);
        false
    }
}

/// A projection whose next look and runner's exit the test sets, counting and timing its
/// looks. The next look is named once: the read after a look takes it. A look reaps a runner
/// that has exited, as [`crate::converge::Last::of`] does: its exit stops owing one.
#[derive(Default)]
struct Owes {
    next: Mutex<Option<Instant>>,
    exit: Mutex<Option<Arc<OwnedFd>>>,
    looks: Arc<AtomicUsize>,
    at: Mutex<Vec<Instant>>,
}

impl Projected for Owes {
    fn event(&self, event: ProjectionEvent) {
        match event {
            ProjectionEvent::Maintenance => {
                let mut exit = self.exit.lock().unwrap();
                let exited = exit.as_ref().is_some_and(|fd| {
                    aterm_uds::ready::any_now(&[fd.as_fd()]).is_ok_and(|ready| !ready.is_empty())
                });
                if exited {
                    exit.take();
                }
            }
            ProjectionEvent::Observe | ProjectionEvent::Recovered => {
                self.at.lock().unwrap().push(Instant::now());
                self.looks.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    fn next_look(&self) -> Option<Instant> {
        self.next.lock().unwrap().take()
    }

    fn wakes(&self) -> Vec<Arc<OwnedFd>> {
        self.exit.lock().unwrap().iter().cloned().collect()
    }
}

/// Run the worker over `project` and `rings` under a private, absent broker, handing `drive`
/// each wait as it begins, until `drive` returns.
fn driving(
    project: &Owes,
    rings: Rings<'_>,
    members: &Mutex<MembersAsked>,
    drive: impl FnOnce(&std::sync::mpsc::Receiver<Wait>),
) {
    let home = aterm_tempfile::tempdir().expect("home");
    let paths = Paths::locate_for_home(home.path());
    let (waits, waiting) = std::sync::mpsc::channel();
    let changes = Hears {
        waits,
        looks: Arc::clone(&project.looks),
    };
    aterm_uds::lan::seam_here(
        aterm_uds::lan::Seam {
            socket: Some(home.path().join("absent-broker.sock")),
            ..aterm_uds::lan::Seam::default()
        },
        || {
            while_hearing(
                &paths,
                members,
                &|_: &Paths| Ok(0),
                &|_| {},
                project,
                rings,
                changes,
                |_: &mut dyn FnMut() -> Status| drive(&waiting),
            );
        },
    );
}

/// The next wait that begins after the worker has taken `looks` looks ([`HANG`] detects a
/// worker that never takes them).
fn after(waiting: &std::sync::mpsc::Receiver<Wait>, looks: usize) -> Wait {
    loop {
        let wait = waiting.recv_timeout(HANG).expect("the worker waits again");
        if wait.1 >= looks {
            return wait;
        }
    }
}

/// With nothing owed the worker waits with no deadline; a ring of its inputs owes a look, and
/// until it is taken each wait runs to the end of the [`LOOK`] that spaces it, never
/// unbounded; a next look the projection names sooner than that spacing waits for it, and is
/// taken at its deadline.
/// NEGATIVE CONTROLS: a round that looks again a [`LOOK`] after each look gives a wait owed
/// nothing a deadline; one that ignores the inputs' ring never takes the second look; one
/// that takes the projection's next look unspaced waits to the instant the test named.
#[test]
fn a_ring_owes_a_look_and_nothing_else_does() {
    let inputs = Doorbell::new().expect("a doorbell");
    let project = Owes::default();
    let members = Mutex::new(MembersAsked::default());
    let rings = Rings {
        inputs: Some(&inputs),
        ..Rings::default()
    };
    driving(&project, rings, &members, |waiting| {
        assert_eq!(
            after(waiting, 1).0,
            None,
            "a wait owed nothing has no deadline"
        );
        let named = Instant::now();
        *project.next.lock().unwrap() = Some(named);
        inputs.ring();
        let deadline = loop {
            let (deadline, looks) = waiting.recv_timeout(HANG).expect("the worker waits");
            if looks >= 2 {
                break deadline;
            }
            assert!(deadline.is_some(), "a look owed waited with no deadline");
        };
        let deadline = deadline.expect("the projection named a next look");
        assert!(
            deadline >= named + LOOK,
            "the projection's next look waited only {:?} past the instant it named",
            deadline.saturating_duration_since(named)
        );
        let (next, _) = after(waiting, 3);
        let third = project.at.lock().unwrap()[2];
        assert!(third >= deadline, "a look came before its deadline");
        assert_eq!(next, None, "nothing owed after it");
    });
}

/// The members' ring ends the wait, and the worker reads its asking again without a look.
/// NEGATIVE CONTROL: a round that owes a look at the members' ring looks a second time.
#[test]
fn the_members_ring_ends_the_wait_and_owes_no_look() {
    let bell = Doorbell::new().expect("a doorbell");
    let project = Owes::default();
    let members = Mutex::new(MembersAsked::default());
    let rings = Rings {
        members: Some(&bell),
        ..Rings::default()
    };
    driving(&project, rings, &members, |waiting| {
        after(waiting, 1);
        bell.ring();
        let (deadline, looks) = waiting.recv_timeout(HANG).expect("the ring ends the wait");
        assert_eq!(
            (deadline, looks),
            (None, 1),
            "the members' ring owes no look"
        );
        assert!(!bell.clear(), "the worker's round took the ring");
    });
}

/// A runner another thread started joins the wait at the projection's ring, which owes no
/// look; its exit then owes one. A next look the projection names at its ring joins the
/// deadline, spaced as every look is, and is taken there.
/// NEGATIVE CONTROLS: a round that leaves the projection's ring out of its wait never hears
/// the runner it did not start; one that owes a look at that ring waits for the spacing; one
/// that reads no next look at that ring waits with no deadline.
#[test]
fn another_threads_runner_joins_the_wait() {
    use std::io::Write as _;
    let bell = Doorbell::new().expect("a doorbell");
    let project = Owes::default();
    let members = Mutex::new(MembersAsked::default());
    let rings = Rings {
        projection: Some(&bell),
        ..Rings::default()
    };
    driving(&project, rings, &members, |waiting| {
        assert_eq!(
            after(waiting, 1).0,
            None,
            "a wait owed nothing has no deadline"
        );
        let (exit, mut exits) = aterm_uds::pipe().expect("an exit's stand-in");
        *project.exit.lock().unwrap() = Some(Arc::new(OwnedFd::from(exit)));
        bell.ring();
        let wait = waiting.recv_timeout(HANG).expect("the ring ends the wait");
        assert_eq!(wait, (None, 1), "the projection's ring owes no look");
        exits.write_all(&[0]).expect("the runner exits");
        after(waiting, 2);
        assert!(
            project.exit.lock().unwrap().is_none(),
            "the exit's look reaped the runner"
        );
        let named = Instant::now();
        *project.next.lock().unwrap() = Some(named);
        bell.ring();
        let (deadline, looks) = waiting.recv_timeout(HANG).expect("the ring ends the wait");
        assert_eq!(looks, 2, "the projection's ring owes no look");
        let deadline = deadline.expect("the named look joined the deadline");
        assert!(deadline >= named, "a deadline before the instant named");
        let (next, _) = after(waiting, 3);
        assert!(
            project.at.lock().unwrap()[2] >= deadline,
            "a look came before its deadline"
        );
        assert_eq!(next, None, "nothing owed after it");
    });
}

/// A runner's exit owes a look, which reaps it; the exit that stays ready until then never
/// turns the wait into a spin.
/// NEGATIVE CONTROL: a round that leaves the projection's wakes out of its wait never takes
/// the second look; one that waits on them while a look is owed spins until it is taken.
#[test]
fn a_runners_exit_owes_a_look() {
    use std::io::Write as _;
    let project = Owes::default();
    let (exit, mut exits) = aterm_uds::pipe().expect("an exit's stand-in");
    *project.exit.lock().unwrap() = Some(Arc::new(OwnedFd::from(exit)));
    let members = Mutex::new(MembersAsked::default());
    driving(&project, Rings::default(), &members, |waiting| {
        after(waiting, 1);
        exits.write_all(&[0]).expect("the runner exits");
        let mut spun = 0_usize;
        loop {
            let (_, looks) = waiting.recv_timeout(HANG).expect("the worker waits again");
            if looks >= 2 {
                break;
            }
            spun += 1;
        }
        // The spacing's wait, and a spurious wake or two: never the thousands a ready exit
        // polled while its look is owed would make.
        assert!(spun < 10, "{spun} waits before the exit's look");
        assert!(
            project.exit.lock().unwrap().is_none(),
            "the look reaped the runner"
        );
    });
}

/// The worker ends as its subscriber does: the stop's bell ends the wait under way, which
/// would otherwise run to its deadline ([`HANG`] detects a wait no bell ended).
#[test]
fn an_ending_subscriber_ends_the_wait_under_way() {
    struct Waits {
        waiting: std::sync::mpsc::Sender<()>,
        rung: Arc<AtomicBool>,
    }
    impl Changes for Waits {
        fn wait(&mut self, _deadline: Option<Instant>, also: &[BorrowedFd<'_>]) -> bool {
            let _ = self.waiting.send(());
            let rung = aterm_uds::ready::any_until(also, Instant::now() + HANG)
                .is_ok_and(|ready| !ready.is_empty());
            self.rung.store(rung, Ordering::SeqCst);
            false
        }
    }
    let home = aterm_tempfile::tempdir().expect("home");
    let paths = Paths::locate_for_home(home.path());
    let members = Mutex::new(MembersAsked::default());
    let (waiting, waits) = std::sync::mpsc::channel();
    let rung = Arc::new(AtomicBool::new(false));
    let changes = Waits {
        waiting,
        rung: Arc::clone(&rung),
    };
    aterm_uds::lan::seam_here(
        aterm_uds::lan::Seam {
            socket: Some(home.path().join("absent-broker.sock")),
            ..aterm_uds::lan::Seam::default()
        },
        || {
            while_hearing(
                &paths,
                &members,
                &|_: &Paths| Ok(0),
                &|_| {},
                &|_: ProjectionEvent| {},
                Rings::default(),
                changes,
                |_: &mut dyn FnMut() -> Status| {
                    waits.recv_timeout(HANG).expect("the worker waits");
                },
            );
        },
    );
    assert!(
        rung.load(Ordering::SeqCst),
        "the subscriber's end did not end the worker's wait"
    );
}

/// A round that fails rests on the worker's stop, and the stop ends the rest ([`HANG`] detects
/// a rest taken anywhere but on the stop). That the rest runs the retry's whole pause in one
/// sleep is `retry_rounds`' own code; this test does not time it.
#[test]
fn a_failed_round_rests_on_the_stop() {
    let ending = Stop::new();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            retry_rounds(
                Instant::now(),
                &ending,
                &|_| {},
                || Err(std::io::Error::other("injected source failure")),
                || {},
            );
        });
        // Set on the way out, a failed assertion's included, so the scope's join ends.
        let ends = Ending(&ending);
        let began = Instant::now();
        while ending.entered() == 0 {
            assert!(
                began.elapsed() < HANG,
                "the failed round never rested on the stop"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(ends);
        worker.join().expect("the worker ends with its stop");
    });
}

/// Answer the first hello at `listener` as a broker does ([`HANG`] detects a worker that
/// never dials).
#[cfg(target_os = "macos")]
fn answer_one_hello(listener: &std::os::unix::net::UnixListener) {
    use aterm_uds::lan::wire::{self, Frame, Reader};
    let (stream, _) = aterm_uds::accept(
        listener,
        aterm_uds::AcceptWait::Until(Instant::now() + HANG),
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("the worker's dial");
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
        |_, looks| {
            waits += 1;
            match waits {
                1 => first,
                2 => {
                    let _ = second.set(looks);
                    false
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(1));
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
        assert!(!changes.wait(
            Some(Instant::now() + HANG),
            &[broker.as_fd(), native.as_fd()]
        ));
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
    let (quiet, _writer) = aterm_uds::pipe().expect("a broker descriptor nobody writes");
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
    let changed = changes.wait(Some(began + most), &[quiet.as_fd()]);
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
    let (quiet, _writer) = aterm_uds::pipe().expect("a broker descriptor nobody writes");
    let most = Duration::from_millis(300);
    for _ in 0..2 {
        let began = Instant::now();
        assert!(!changes.wait(Some(began + most), &[quiet.as_fd()]));
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
    let (quiet, _writer) = aterm_uds::pipe().expect("a broker descriptor nobody writes");
    assert!(!changes.wait(
        Some(Instant::now() + Duration::from_millis(300)),
        &[quiet.as_fd()]
    ));
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
        changes.wait(Some(Instant::now() + HANG), &[quiet.as_fd()]),
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
    let (ready, mut ring) = aterm_uds::pipe().expect("a broker descriptor");
    ring.write_all(&[1]).expect("the broker's arrival");
    let (mut changes, _kernel) = stand_in();
    let began = Instant::now();
    assert!(
        !changes.wait(Some(Instant::now() + HANG), &[ready.as_fd()]),
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
        !failed.wait(Some(Instant::now() + HANG), &[ready.as_fd()]),
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
        queued.wait(Some(Instant::now()), &[ready.as_fd()]),
        "a queued route change is consumed even when another edge is already due"
    );
    assert!(
        !queued.wait(Some(Instant::now()), &[ready.as_fd()]),
        "the route was drained and the broker still owns its unread bytes"
    );
    let unrelated: Vec<u8> = [24_u32, 1, 1, 3, 37, 1]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect();
    kernel.send(&unrelated).unwrap();
    assert!(!queued.wait(Some(Instant::now()), &[]));
}
