// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read, Write};
use std::sync::Arc;

fn scratch() -> aterm_tempfile::TempDir {
    crate::test_scratch::short_dir("a listener's socket")
}

#[test]
fn queue_pending_reads_the_kernel_queue() {
    let dir = scratch();
    let path = dir.path().join("q.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    assert_eq!(
        queue_pending(&listener),
        Some(0),
        "an idle listener queues nothing"
    );
    let clients: Vec<_> = (0..3)
        .map(|_| CtlStream::connect(&path).expect("connect"))
        .collect();
    let expected = if cfg!(target_os = "macos") { 3 } else { 1 };
    assert_eq!(queue_pending(&listener), Some(expected));
    for _ in 0..3 {
        drop(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept"),
        );
    }
    assert_eq!(queue_pending(&listener), Some(0), "a drained queue reads 0");
    drop(clients);
}

/// How generation 0 hears its listener's readiness.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hearing {
    /// Both threads hear it.
    Both,
    /// The accept thread's park is deaf to it (a lost `poll(2)` wakeup in that thread).
    AcceptDeaf,
    /// Neither thread hears it: a queue whose readiness never fires.
    Neither,
}

/// The supervisor over a fresh socket at `path`, at `pace`, with its seams; `hearing` says who
/// hears generation 0's listener from its first wait.
fn supervised(
    path: &std::path::Path,
    pace: Pace,
    hearing: Hearing,
) -> (Arc<Stop>, Seams, std::thread::JoinHandle<()>) {
    let listener = CtlListener::bind(path).expect("bind");
    let published = path.to_str().expect("utf8").to_string();
    let mut expected = socket_file_id(&published);
    let rebind_path = published.clone();
    let mut supervisor = Supervisor::new(
        listener,
        move || rebind_socket(&rebind_path, &mut expected),
        Endpoint::default(),
    );
    let seams = Seams {
        withhold: Arc::new(AtomicU64::new(u64::MAX)),
        withholding: Arc::new(AtomicU64::new(u64::MAX)),
        park: Arc::new(AtomicU64::new(u64::MAX)),
        parked: Arc::new(AtomicU64::new(u64::MAX)),
        deaf: Arc::new(AtomicU64::new(if hearing == Hearing::Both {
            u64::MAX
        } else {
            0
        })),
        blind: Arc::new(AtomicU64::new(if hearing == Hearing::Neither {
            0
        } else {
            u64::MAX
        })),
        accept_resting: Arc::new(AtomicU64::new(u64::MAX)),
        watch_resting: Arc::new(AtomicBool::new(false)),
        pace,
    };
    supervisor.seams = Some(seams.clone());
    let stop = Arc::new(Stop::new());
    let stopping = stop.clone();
    let thread = std::thread::spawn(move || {
        supervisor.run(
            |mut stream| {
                let _ = stream.write_all(b"OK served\n");
            },
            crate::control::turn_away_at_limit,
            &stopping,
        );
    });
    (stop, seams, thread)
}

fn arm(stream: &CtlStream, patience: Duration) -> std::io::Result<()> {
    match stream.set_read_timeout(Some(patience)) {
        Err(error)
            if cfg!(target_vendor = "apple")
                && error.kind() == std::io::ErrorKind::InvalidInput
                && error.raw_os_error().is_some() =>
        {
            Ok(())
        }
        other => other,
    }
}

fn ask(path: &std::path::Path, patience: Duration) -> std::io::Result<String> {
    let stream = CtlStream::connect(path)?;
    arm(&stream, patience)?;
    read_reply(&stream)
}

fn read_reply(stream: &CtlStream) -> std::io::Result<String> {
    use std::io::BufRead as _;
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply)
}

fn served_after_rebind(path: &std::path::Path, before: Option<(u64, u64)>) -> String {
    let published = path.to_str().expect("utf8");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the watchdog to rebind the published path",
        || (socket_file_id(published) != before).then_some(()),
    );
    ask(path, aterm_spec::wait::MUST_HAPPEN).expect("the fresh socket answers")
}

fn assert_rebound_owner_only(path: &std::path::Path, before: Option<(u64, u64)>) {
    assert_ne!(
        socket_file_id(path.to_str().expect("utf8")),
        before,
        "the published path must name the fresh socket"
    );
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    };
    assert_eq!(mode, 0o600, "the rebound socket is owner-only");
}

#[test]
fn a_stalled_listener_is_detected_and_rebound() {
    let dir = scratch();
    let path = dir.path().join("w.sock");
    let pace = Pace {
        tick: Duration::from_millis(50),
        wedge_after: Duration::from_millis(300),
        rest_look: None,
    };
    let (stop, seams, thread) = supervised(&path, pace, Hearing::Both);
    let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);

    assert_eq!(
        ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("healthy ask"),
        "OK served\n",
        "a healthy listener serves"
    );
    // The reply is written from inside the drain pass.  Do not arm the seam until that pass has
    // finished, or the peer planted below can be consumed by the healthy turn before the accept
    // loop observes `withhold`.
    until_resting(&seams, 0, "the healthy listener to return to rest");

    seams.withhold.store(0, Ordering::Release);
    let inode_before = socket_file_id(path.to_str().expect("utf8"));
    // The queued peer wakes the parked thread into the turn that withholds it.
    let queued = CtlStream::connect(&path).expect("connect into the wedged queue");
    arm(&queued, aterm_spec::wait::MUST_HAPPEN).expect("deadline");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the accept thread to reach its withhold check",
        || (seams.withholding.load(Ordering::Acquire) == 0).then_some(()),
    );

    let mut sink = String::new();
    let released = (&queued).read_to_string(&mut sink);
    assert!(
        released.is_err() || sink.is_empty(),
        "the wedged socket served a peer it was withholding: {sink:?}"
    );

    assert_eq!(served_after_rebind(&path, inode_before), "OK served\n");
    assert_rebound_owner_only(&path, inode_before);
    assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

    stop.set();
    thread.join().expect("supervisor thread");
}

#[test]
fn a_listener_stuck_inside_accept_is_detected_and_rebound() {
    let dir = scratch();
    let path = dir.path().join("p.sock");
    let pace = Pace {
        tick: Duration::from_millis(100),
        wedge_after: Duration::from_secs(1),
        rest_look: None,
    };
    let (stop, seams, thread) = supervised(&path, pace, Hearing::Both);
    let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);
    assert_eq!(
        ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("healthy ask"),
        "OK served\n",
        "a healthy listener serves"
    );
    // The reply is written from inside the drain pass.  Wait for that pass to finish so the peer
    // planted below cannot be consumed before the accept loop observes `park`.
    until_resting(&seams, 0, "the healthy listener to return to rest");

    seams.park.store(0, Ordering::Release);
    let inode_before = socket_file_id(path.to_str().expect("utf8"));
    // The queued peer wakes the parked thread into the turn that sticks before accepting it.
    let queued = CtlStream::connect(&path).expect("connect into the stuck queue");
    arm(&queued, aterm_spec::wait::MUST_HAPPEN).expect("deadline");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "generation 0 to reach its park",
        || (seams.parked.load(Ordering::Acquire) == 0).then_some(()),
    );

    assert_eq!(served_after_rebind(&path, inode_before), "OK served\n");
    assert_rebound_owner_only(&path, inode_before);
    assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

    seams.park.store(u64::MAX, Ordering::Release);
    assert_eq!(
        read_reply(&queued).expect("the retired thread's answer"),
        "OK served\n",
        "the retired thread served its queue"
    );

    stop.set();
    thread.join().expect("supervisor thread");
}

/// How far past the count of `/dev/fd`'s entries (the descriptors this process holds, the one
/// reading the directory included) [`AtLimit::hold`] sets the soft limit, where that is below the
/// limit it had. However many descriptors that leaves free below the limit, it then fills them.
const SPARE_DESCRIPTORS: usize = 16;

/// This process held at its descriptor limit: its soft limit lowered to a few past the count of
/// descriptors it holds, and every descriptor free below it filled with `/dev/null`, so whatever
/// opens one next is refused `EMFILE` until a filler is given up. Dropping it frees the fillers
/// and restores the limit. Only a test run alone in its own process ([`crate::test_alone::alone`])
/// holds it: every thread of the process is held at the limit with the test's.
struct AtLimit {
    fillers: Vec<std::fs::File>,
    restore: libc::rlimit,
}

impl AtLimit {
    fn hold() -> Self {
        let mut restore = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut restore) },
            0
        );
        let open = std::fs::read_dir("/dev/fd")
            .expect("the descriptor directory")
            .count();
        let lowered = libc::rlimit {
            rlim_cur: libc::rlim_t::try_from(open + SPARE_DESCRIPTORS)
                .expect("a descriptor limit")
                .min(restore.rlim_cur),
            rlim_max: restore.rlim_max,
        };
        // SAFETY: a change to this process alone (the test runs alone in it) that keeps its hard
        // limit.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lowered) },
            0
        );
        let mut fillers = Vec::new();
        let refused = loop {
            match std::fs::File::open("/dev/null") {
                Ok(filler) => fillers.push(filler),
                Err(error) => break error,
            }
        };
        assert_eq!(
            refused.raw_os_error(),
            Some(libc::EMFILE),
            "the fill ends at the descriptor limit: {refused}"
        );
        assert!(
            !fillers.is_empty(),
            "no descriptor was free below the lowered limit"
        );
        Self { fillers, restore }
    }

    /// Give up one filler, connect a client to `path` in its place and send its request line,
    /// which no one reads: the process is at its limit again. A listener serving meanwhile may
    /// turn the client away and close the connection before the line is written: the write then
    /// fails as one to a peer gone does (with one of the kinds aterm-ctl's `is_peer_drop` takes
    /// for a peer's drop), and what the listener said is still there to read, as aterm-ctl reads
    /// it.
    fn connect(&mut self, path: &std::path::Path) -> CtlStream {
        drop(self.fillers.pop().expect("a filler to give up"));
        let client = CtlStream::connect(path).expect("a client in the descriptor given up");
        let mut out = &client;
        if let Err(error) = out.write_all(b"AUTH -\n") {
            assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::NotConnected
                ),
                "the client's request line fails only where its peer closed first: {error}"
            );
        }
        client
    }
}

impl Drop for AtLimit {
    fn drop(&mut self) {
        self.fillers.clear();
        // SAFETY: a valid resource id and the limit this process had, as read.
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const self.restore) };
    }
}

/// What `stream` reads to the end of its connection, and how it ended: `Ok` at EOF, else the
/// read's error. The read waits a hang detector, never a latency budget.
fn told(stream: &CtlStream) -> (String, std::io::Result<()>) {
    arm(stream, aterm_spec::wait::MUST_HAPPEN).expect("a read deadline");
    let mut bytes = Vec::new();
    let mut reader = stream;
    let end = reader.read_to_end(&mut bytes).map(drop);
    (String::from_utf8_lossy(&bytes).into_owned(), end)
}

/// Whether a connection's end is the peer's close: EOF, or the reset a close over unread bytes
/// can raise.
fn closed(end: &std::io::Result<()>) -> bool {
    end.as_ref()
        .err()
        .is_none_or(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
}

/// Held at its descriptor limit, the supervised listener gives its reserve up to accept a client
/// below any cap, has the control server's turn-away tell it busy (not hang it up unanswered),
/// closes it, and takes the reserve again, so the next client at the limit is told busy too;
/// with descriptors free again it serves. The test runs alone in a child of the test binary,
/// which it holds at the limit by lowering its soft limit and filling every descriptor free
/// below it, so the count of descriptors the process holds otherwise does not matter.
#[test]
fn a_listener_out_of_descriptors_tells_a_client_busy_from_its_reserve() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = scratch();
    let path = dir.path().join("r.sock");
    let (stop, seams, thread) = supervised(
        &path,
        Pace {
            tick: Duration::from_millis(50),
            wedge_after: Duration::from_secs(3600),
            rest_look: None,
        },
        Hearing::Both,
    );
    until_resting(&seams, 0, "the listener to come to rest");
    assert_eq!(
        ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("an ask with descriptors free"),
        "OK served\n"
    );
    until_resting(&seams, 0, "the listener to rest after serving");
    let busy = HEALTH.busy_replies.load(Ordering::Relaxed);
    let accepts = HEALTH.accepts.load(Ordering::Relaxed);

    let mut limit = AtLimit::hold();
    let mut clients = Vec::new();
    for client in 0..2 {
        let stream = limit.connect(&path);
        let (said, end) = told(&stream);
        assert_eq!(
            said,
            aterm_types::control_verbs::CONTROL_BUSY_LINE,
            "client {client} at the descriptor limit is told busy, not hung up unanswered \
             (its connection then ended {end:?})"
        );
        assert!(
            closed(&end),
            "client {client}, told busy, is closed: {end:?}"
        );
        // Held, so its descriptor stays taken and the process stays at its limit.
        clients.push(stream);
        until_resting(
            &seams,
            0,
            "the listener to rest after turning a client away",
        );
    }
    assert_eq!(
        HEALTH.busy_replies.load(Ordering::Relaxed) - busy,
        2,
        "each client turned away is a busy reply"
    );
    assert_eq!(
        HEALTH.accepts.load(Ordering::Relaxed) - accepts,
        2,
        "each client turned away is an accept"
    );

    drop(clients);
    drop(limit);
    assert_eq!(
        ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("an ask with descriptors free again"),
        "OK served\n",
        "with descriptors free again the listener serves"
    );
    stop.set();
    thread.join().expect("supervisor thread");
}

/// A reserve lost (here its descriptor taken by a filler, as another thread's open would take
/// it) while no descriptor is free makes no accept: the client waits queued, where an accept with
/// no descriptor for it would have closed it unanswered on macOS. Once a descriptor is free the
/// reserve is taken again before the accept, and the client is told busy.
#[test]
fn a_lost_reserve_makes_no_accept_until_a_descriptor_is_free() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = scratch();
    let path = dir.path().join("l.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    let reserve = Reserve::take();
    assert!(held(&reserve), "the reserve is taken with descriptors free");

    let mut limit = AtLimit::hold();
    let client = limit.connect(&path);
    drop(
        reserve
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .file
            .take(),
    );
    limit
        .fillers
        .push(std::fs::File::open("/dev/null").expect("the filler in the reserve's descriptor"));
    let turn_away = crate::control::turn_away_at_limit;
    match reserve.accept(&listener, &turn_away) {
        Ok(Accepted::Deferred(error)) => assert_eq!(
            error.raw_os_error(),
            Some(libc::EMFILE),
            "deferred for want of a descriptor: {error}"
        ),
        other => panic!("a lost reserve with no descriptor free made an accept: {other:?}"),
    }
    assert!(!held(&reserve), "no descriptor was free for the reserve");
    assert_eq!(
        queue_pending(&listener),
        Some(1),
        "the client waits queued, not closed"
    );

    drop(limit.fillers.pop());
    match reserve.accept(&listener, &turn_away) {
        Ok(Accepted::TurnedAway) => {}
        other => panic!(
            "one descriptor free takes the reserve, and the client is turned away: {other:?}"
        ),
    }
    let (said, end) = told(&client);
    assert_eq!(
        said,
        aterm_types::control_verbs::CONTROL_BUSY_LINE,
        "the client that waited is told busy (its connection then ended {end:?})"
    );
    assert!(closed(&end), "the client told busy is closed: {end:?}");
    assert!(held(&reserve), "the reserve is held again");
    drop(limit);
}

/// Whether `reserve` holds its descriptor now.
fn held(reserve: &Reserve) -> bool {
    reserve
        .held
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .file
        .is_some()
}

/// An accept that waits for the spawn lock (here behind the test's own hold of
/// [`aterm_uds::spawn_lock::spawning`], as behind a `posix_spawn` in flight) still holds the
/// reserve while it waits: the reserve is given up only under the lock's exclusive hold, so a
/// descriptor opened meanwhile at the limit is refused `EMFILE` and cannot take the reserve's
/// place. With the spawn done the queued client is accepted into the reserve's descriptor, told
/// busy, and the reserve is held again. A reserve given up before the wait would have let that
/// open take its descriptor, and the accept after the wait would have found none (macOS then
/// drops the client unanswered).
#[cfg(target_vendor = "apple")]
#[test]
fn an_accept_waiting_out_a_spawn_keeps_its_reserve_until_it_holds_the_lock() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = scratch();
    let path = dir.path().join("w.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    let reserve = Reserve::take();
    assert!(held(&reserve), "the reserve is taken with descriptors free");

    let mut limit = AtLimit::hold();
    let client = limit.connect(&path);
    let waiting = aterm_uds::spawn_lock::seam::exclusive_waiters();
    let spawn = aterm_uds::spawn_lock::spawning();
    let (opened, accepted) = std::thread::scope(|scope| {
        let accepting =
            scope.spawn(|| reserve.accept(&listener, &crate::control::turn_away_at_limit));
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the accept to wait for the spawn lock",
            || (aterm_uds::spawn_lock::seam::exclusive_waiters() > waiting).then_some(()),
        );
        let opened = std::fs::File::open("/dev/null");
        drop(spawn);
        (opened, accepting.join().expect("the accepting thread"))
    });
    assert_eq!(
        opened.as_ref().err().and_then(std::io::Error::raw_os_error),
        Some(libc::EMFILE),
        "an accept waiting for the spawn lock still holds its reserve, so an open meanwhile \
         finds no descriptor: {opened:?}"
    );
    match accepted {
        Ok(Accepted::TurnedAway) => {}
        other => panic!(
            "with the spawn done the client is accepted into the reserve and turned away: \
             {other:?}"
        ),
    }
    let (said, end) = told(&client);
    assert_eq!(
        said,
        aterm_types::control_verbs::CONTROL_BUSY_LINE,
        "the client queued behind the spawn is told busy (its connection then ended {end:?})"
    );
    assert!(closed(&end), "the client told busy is closed: {end:?}");
    assert!(held(&reserve), "the reserve is held again");
    drop(limit);
}

/// Both threads rest: the accept thread of `generation` in its park with no deadline and the
/// watchdog disarmed. A hang detector, never a latency budget.
fn until_resting(seams: &Seams, generation: u64, what: &str) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
        (seams.accept_resting.load(Ordering::Acquire) == generation
            && seams.watch_resting.load(Ordering::Acquire))
        .then_some(())
    });
}

/// At rest the accept thread waits with no deadline and the watchdog disarms, with no rest look,
/// and a connection arriving is accepted by the accept thread's own wake (the watchdog accepts
/// nothing), after which both come to rest again.
#[test]
fn a_listener_at_rest_waits_with_no_deadline_and_serves_what_arrives() {
    let dir = scratch();
    let path = dir.path().join("i.sock");
    let (stop, seams, thread) = supervised(
        &path,
        Pace {
            tick: Duration::from_millis(50),
            wedge_after: Duration::from_secs(3600),
            rest_look: None,
        },
        Hearing::Both,
    );
    until_resting(&seams, 0, "the listener to come to rest");
    for _ in 0..3 {
        assert_eq!(
            ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("an ask at rest"),
            "OK served\n",
            "a connection arriving at rest is served"
        );
        until_resting(&seams, 0, "the listener to rest again after serving");
    }
    stop.set();
    thread.join().expect("supervisor thread");
}

/// A parked accept thread that misses its listener's wake (here deaf to it, as a lost `poll(2)`
/// wakeup would leave it) is caught by the watchdog's own poll of the queue, judged stuck and
/// rebound; the retired thread, told to quit, serves the peer it missed on its way out.
#[test]
fn a_parked_listener_that_misses_its_wake_is_detected_and_rebound() {
    missed_wake_is_rebound(
        "d.sock",
        Hearing::AcceptDeaf,
        // No rest look: the watchdog's poll of the listener alone must catch it.
        None,
    );
}

/// A queued connection whose readiness never fires for either thread (a lost wakeup on the socket
/// itself) is caught by the disarmed watchdog's rest look, which reads the kernel's count of the
/// queue (a zero-timeout `poll(2)` off macOS, which recomputes readiness from the queue), and
/// judged and rebound as above.
#[test]
fn a_queue_whose_readiness_never_fires_is_caught_by_the_rest_look() {
    missed_wake_is_rebound("s.sock", Hearing::Neither, Some(Duration::from_millis(200)));
}

fn missed_wake_is_rebound(name: &str, hearing: Hearing, rest_look: Option<Duration>) {
    let dir = scratch();
    let path = dir.path().join(name);
    let (stop, seams, thread) = supervised(
        &path,
        Pace {
            tick: Duration::from_millis(50),
            wedge_after: Duration::from_millis(300),
            rest_look,
        },
        hearing,
    );
    let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);
    until_resting(&seams, 0, "the deaf listener to come to rest");
    let inode_before = socket_file_id(path.to_str().expect("utf8"));
    let missed = CtlStream::connect(&path).expect("connect into the deaf queue");
    arm(&missed, aterm_spec::wait::MUST_HAPPEN).expect("deadline");

    assert_eq!(served_after_rebind(&path, inode_before), "OK served\n");
    assert_rebound_owner_only(&path, inode_before);
    assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);
    assert_eq!(
        read_reply(&missed).expect("the retired thread's answer"),
        "OK served\n",
        "the retired thread served the peer it missed"
    );
    stop.set();
    thread.join().expect("supervisor thread");
}

fn reading(progress: u64, accepts: u64, pending: Option<u64>) -> Reading {
    Reading {
        progress,
        accepts,
        pending,
    }
}

#[test]
fn the_watch_judges_stuck_stalled_and_healthy() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Pace::PRODUCTION.tick * u32::try_from(s).expect("a few ticks");

    let mut watch = Watch::new(t0);
    for s in 1..=10 {
        assert_eq!(watch.observe(reading(s, s, Some(1)), at(s)), None);
    }

    let mut watch = Watch::new(t0);
    assert_eq!(watch.observe(reading(1, 0, Some(0)), at(1)), None);
    assert_eq!(
        watch.observe(reading(1, 0, Some(0)), at(60)),
        None,
        "one stale tick after a long gap is a suspension, not a wedge"
    );
    assert_eq!(watch.observe(reading(1, 0, Some(0)), at(61)), None);
    assert!(matches!(
        watch.observe(reading(1, 0, Some(0)), at(62)),
        Some(Wedge::Stuck { .. })
    ));

    let mut watch = Watch::new(t0);
    assert_eq!(watch.observe(reading(1, 7, Some(5)), at(1)), None);
    assert_eq!(watch.observe(reading(2, 7, Some(5)), at(2)), None);
    assert_eq!(watch.observe(reading(3, 7, Some(5)), at(3)), None);
    assert_eq!(watch.observe(reading(4, 7, Some(5)), at(4)), None);
    assert_eq!(
        watch.observe(reading(5, 7, Some(5)), at(5)),
        Some(Wedge::Stalled {
            queued: 5,
            for_ms: 3000
        })
    );
    assert_eq!(watch.observe(reading(6, 8, Some(5)), at(6)), None);

    watch.rebound(at(6));
    for s in 7..=15 {
        assert_eq!(watch.observe(reading(s, 0, Some(5)), at(s)), None, "{s}");
    }
    assert!(matches!(
        watch.observe(reading(16, 0, Some(5)), at(16)),
        Some(Wedge::Stalled { .. })
    ));
}

#[test]
fn shutting_accepted_connections_down_reaches_a_client_that_half_closed() {
    let dir = scratch();
    let path = dir.path().join("h.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    let open = CtlStream::connect(&path).expect("connect");
    let (open_held, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");
    let half_closed = CtlStream::connect(&path).expect("connect");
    let (half_closed_held, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");
    (&half_closed)
        .write_all(b"AUTH ok\nawait idle\n")
        .expect("the request");
    half_closed
        .shutdown(std::net::Shutdown::Write)
        .expect("the client half-closes after its request");
    drop(listener);

    assert_eq!(
        shutdown_accepted(&[path]),
        2,
        "both accepted connections are shut down"
    );
    for (client, what) in [
        (&open, "the open client"),
        (&half_closed, "the half-closed client"),
    ] {
        client.set_nonblocking(true).expect("non-blocking");
        assert_eq!(
            (&*client).read(&mut [0u8; 16]).map_err(|e| e.kind()),
            Ok(0),
            "{what} reads its end-of-file"
        );
    }
    drop((open_held, half_closed_held));
}

#[test]
fn rebind_refuses_a_path_it_does_not_own() {
    let dir = scratch();
    let path = dir.path().join("o.sock");
    let published = path.to_str().expect("utf8").to_string();
    let ours = CtlListener::bind(&path).expect("bind ours");
    let mut expected = socket_file_id(&published);
    std::fs::remove_file(&path).expect("unlink ours");
    let theirs = CtlListener::bind(&path).expect("someone else binds the path");
    let theirs_id = socket_file_id(&published);
    assert!(rebind_socket(&published, &mut expected).is_none());
    assert_eq!(
        socket_file_id(&published),
        theirs_id,
        "their socket keeps the path"
    );
    drop((ours, theirs));
}

/// The listener's interrupt wakeups at rest: the supervisor at the production pace, one healthy
/// ask, then `ATERM_MEASURE_REST_SECS` (65 by default) with nothing connecting, read by
/// `proc_pid_rusage` for this whole test process (the measuring thread's own sleep included). A
/// manual measurement, never a timing gate (ATERM-KI-1105-20).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual idle measurement of the control listener at rest, not a timing gate"]
fn listener_rest_wakeups() {
    fn wakeups() -> (u64, u64) {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: a V4 buffer of the right size for this process's own usage.
        let rc = unsafe {
            libc::proc_pid_rusage(
                i32::try_from(std::process::id()).expect("a pid"),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: proc_pid_rusage filled it.
        let info = unsafe { info.assume_init() };
        (info.ri_interrupt_wkups, info.ri_pkg_idle_wkups)
    }
    let span = Duration::from_secs(
        std::env::var("ATERM_MEASURE_REST_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(65),
    );
    let dir = scratch();
    let path = dir.path().join("r.sock");
    let (stop, _seams, thread) = supervised(&path, Pace::PRODUCTION, Hearing::Both);
    assert_eq!(
        ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("healthy ask"),
        "OK served\n"
    );
    std::thread::sleep(Duration::from_secs(2));
    let (wake0, idle0) = wakeups();
    std::thread::sleep(span);
    let (wake1, idle1) = wakeups();
    eprintln!(
        "control listener at rest: span_s={} interrupt_wkups={} pkg_idle_wkups={}",
        span.as_secs(),
        wake1 - wake0,
        idle1 - idle0
    );
    stop.set();
    thread.join().expect("supervisor thread");
}

/// One field of a text `metrics` fragment.
fn text_field<'a>(text: &'a str, key: &str) -> &'a str {
    let at = text
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("no {key}= in {text}"));
    let value = &text[at + key.len() + 2..];
    value.split(' ').next().unwrap_or(value)
}

#[test]
fn the_request_ledger_counts_each_line_under_its_peer_and_its_verb() {
    for _ in 0..3 {
        note_request("zq ledger law", Some("status"));
    }
    note_request("zq ledger law", Some("lease"));
    note_request("zq ledger law", None);
    let text = crate::metrics::wire::render_text(|s| {
        emit_metrics(&crate::metrics::wire::ReadCtx::now(), s);
    });
    let ledger = text_field(&text, "control_requests");
    let mine: Vec<&str> = ledger
        .split(',')
        .filter(|pair| pair.starts_with("zq%20ledger%20law/"))
        .collect();
    assert_eq!(
        mine,
        [
            "zq%20ledger%20law/status:3",
            "zq%20ledger%20law/lease:1",
            "zq%20ledger%20law/unknown:1"
        ],
        "busiest first, then by name: {ledger}"
    );
}

#[test]
fn a_peer_is_named_by_its_lane_then_its_program_and_an_aterm_command_by_its_subcommand_words() {
    const OWN: u32 = 100;
    let comm = |pid: u32| match pid {
        OWN | 7 | 8 | 9 => Some("aterm".to_string()),
        20 => Some("claude".to_string()),
        _ => None,
    };
    let words = |argv: &[&str]| Some(argv.iter().map(|w| w.as_bytes().to_vec()).collect());
    let argv = |pid: u32| match pid {
        7 => words(&["/opt/aterm", "harness", "tap"]),
        8 => words(&["aterm", "ctl", "@self", "status"]),
        9 => words(&["aterm", "ctl", "status", "extra", "words"]),
        20 => words(&["claude", "code", "run"]),
        _ => None,
    };
    let label = |lane, pid| peer_label(lane, pid, OWN, &comm, &argv);
    assert_eq!(label(Some("fleet"), Some(20)), "fleet", "the lane wins");
    assert_eq!(label(None, None), "indirect");
    assert_eq!(label(None, Some(OWN)), "self");
    assert_eq!(label(None, Some(999)), "unreadable");
    assert_eq!(
        label(None, Some(20)),
        "claude",
        "another program's argv is not read"
    );
    assert_eq!(label(None, Some(7)), "aterm harness tap");
    assert_eq!(
        label(None, Some(8)),
        "aterm ctl",
        "a selector ends the words"
    );
    assert_eq!(
        label(None, Some(9)),
        "aterm ctl status",
        "two words at most"
    );
    let unread = |_: u32| None;
    assert_eq!(peer_label(None, Some(7), OWN, &comm, &unread), "aterm");
}
