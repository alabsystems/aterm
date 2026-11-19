// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::database::{Change, Command};
use aterm_json::json;
use std::net::TcpListener;

#[test]
fn deadline_worker_does_not_restart_an_expired_exchange() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    tcp.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let creator = thread::current().id();
    let started = Instant::now();
    let budget = aterm_spec::wait::MUST_HAPPEN * 10;
    let deadline = Deadline::arm_with_clock(&tcp, Patience::absolute(budget), move || {
        if thread::current().id() == creator {
            started
        } else {
            started + budget * 2
        }
    })
    .unwrap();
    let mut byte = [0];
    let read = (&tcp).read(&mut byte);
    assert!(
        matches!(&read, Ok(0))
            || read.as_ref().is_err_and(|error| !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )),
        "an expired exchange must close without a new timeout: {read:?}"
    );
    drop(deadline);
}

#[test]
fn deadline_worker_judges_queued_completion_at_the_original_deadline() {
    let budget = aterm_spec::wait::MUST_HAPPEN * 10;
    for completed_after in [budget - Duration::from_nanos(1), budget] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let creator = thread::current().id();
        let started = Instant::now();
        let (release_worker, released) = mpsc::channel::<()>();
        let deadline = Deadline::arm_with_clock(&tcp, Patience::absolute(budget), move || {
            if thread::current().id() == creator {
                started
            } else {
                released
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .expect("the queued completion releases its deadline worker");
                started + budget * 2
            }
        })
        .unwrap();
        // Queue the completion before the delayed worker can read it.
        deadline.done.send((started + completed_after, 0)).unwrap();
        release_worker.send(()).unwrap();
        drop(deadline);
        let mut byte = [0];
        if completed_after < budget {
            peer.write_all(b"x").unwrap();
            assert_eq!((&tcp).read(&mut byte).unwrap(), 1);
            assert_eq!(byte, [b'x']);
        } else {
            let read = (&tcp).read(&mut byte);
            assert!(
                matches!(&read, Ok(0))
                    || read.as_ref().is_err_and(|error| !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )),
                "completion at the deadline must not cancel expiry: {read:?}"
            );
        }
    }
}

/// A completion queued for a late watcher, whose stream moved after the watcher's last look, is
/// judged by that move: past a window from the exchange's start, it is let be; one whose stream
/// did not move since the last look is cut. NEGATIVE CONTROL, observed: judging the completion
/// by the last look's move alone (`finished` taking `moved` whatever the completion's count)
/// cuts the first exchange: `` a completion after a move must keep the exchange: Ok(0) ``.
#[test]
fn a_queued_completion_after_progress_is_judged_by_that_progress() {
    let window = aterm_spec::wait::MUST_HAPPEN * 10;
    let patience = Patience {
        window,
        guard: window * 10,
    };
    for moves in [1, 0] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let creator = thread::current().id();
        let started = Instant::now();
        let (release_worker, released) = mpsc::channel::<()>();
        let deadline = Deadline::arm_with_clock(&tcp, patience, move || {
            if thread::current().id() == creator {
                started
            } else {
                // Its first look comes after the completion is queued.
                let _ = released.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
                started + window * 2
            }
        })
        .unwrap();
        // Finished half a window after the window from its start, its stream moved since the
        // last look, or not.
        deadline
            .done
            .send((started + window + window / 2, moves))
            .unwrap();
        release_worker.send(()).unwrap();
        drop(deadline);
        let mut byte = [0];
        if moves > 0 {
            peer.write_all(b"x").unwrap();
            let read = (&tcp).read(&mut byte);
            assert!(
                matches!(read, Ok(1)),
                "a completion after a move must keep the exchange: {read:?}"
            );
            assert_eq!(byte, [b'x']);
        } else {
            let read = (&tcp).read(&mut byte);
            assert!(
                matches!(&read, Ok(0))
                    || read.as_ref().is_err_and(|error| !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )),
                "a completion a window after the last move must not cancel expiry: {read:?}"
            );
        }
    }
}

/// [`finished`] over a scripted exchange: a clock that moves only while the watcher waits,
/// the stream's moves by that clock, and the exchange's completion at `completion` (its time
/// from the start and its moves then), or none. Answers whether the watcher let the exchange
/// be, and its clock when it decided. A watcher that waits past four guards never cuts it.
fn scripted(
    patience: Patience,
    moves: impl Fn(Duration) -> u64,
    completion: Option<(Duration, u64)>,
) -> (bool, Duration) {
    let started = Instant::now();
    let clock = std::cell::Cell::new(Duration::ZERO);
    let kept = finished(
        patience,
        started,
        || started + clock.get(),
        || moves(clock.get()),
        |within| {
            let until = clock.get() + within;
            if let Some((at, count)) = completion
                && at <= until
            {
                clock.set(clock.get().max(at));
                return Ok((started + at, count));
            }
            clock.set(until);
            assert!(
                clock.get() <= patience.guard * 4,
                "the watcher never cut the exchange"
            );
            Err(mpsc::RecvTimeoutError::Timeout)
        },
    );
    (kept, clock.get())
}

/// The count of half windows elapsed at `at`, to `last`: a stream that moves every half window
/// until `last`, then never again.
fn every_half(window: Duration, last: Duration) -> impl Fn(Duration) -> u64 {
    move |at| (at.min(last).as_nanos() / (window / 2).as_nanos()) as u64
}

/// An exchange whose stream moves every half window is let be for ten windows; once it stops
/// moving it is cut a window after the look that saw its last move, so within a quarter window
/// of a window after that move. NEGATIVE CONTROL, observed: a look that does not take a new
/// count as a move (`finished` keeping `moved` at the start) cuts it at the first window:
/// `` cut at 60s, not a window after its last move ``.
#[test]
fn a_deadline_cuts_an_exchange_only_after_a_window_without_progress() {
    let window = Duration::from_secs(60);
    let patience = Patience {
        window,
        guard: window * 100,
    };
    let (kept, cut) = scripted(patience, every_half(window, window * 10), None);
    assert!(!kept, "a stream that stopped moving is cut");
    assert!(
        cut >= window * 11 && cut <= window * 11 + window / 4,
        "cut at {cut:?}, not a window after its last move"
    );
}

/// An exchange whose stream moves at every look is cut at its guard. NEGATIVE CONTROL, observed:
/// without the guard (`watch` expiring only a window after the last move) the watcher never cuts
/// it: `` the watcher never cut the exchange ``.
#[test]
fn a_deadline_cuts_a_working_exchange_at_its_guard() {
    let window = Duration::from_secs(60);
    let patience = Patience {
        window,
        guard: window * 3,
    };
    let (kept, cut) = scripted(patience, every_half(window / 2, Duration::MAX), None);
    assert_eq!((kept, cut), (false, window * 3));
}

/// An absolute deadline is today's rule whatever the stream moves: one wait from the start to
/// `within`, an exchange that finished before it let be and one that did not cut there, a
/// stream moving at every instant or not. NEGATIVE CONTROL, observed: an absolute patience whose
/// guard outlasts its window (`Patience::absolute` giving `within * 2`) looks four times a window
/// and lets a moving exchange run past it: `` moved at 0ns, looked at 0ns `` (`Some(15s)` for
/// `Some(60s)`).
#[test]
fn an_absolute_deadline_ignores_progress() {
    let within = Duration::from_secs(60);
    let absolute = Patience::absolute(within);
    let started = Instant::now();
    let nanosecond = Duration::from_nanos(1);
    for moved in [Duration::ZERO, within / 2, within, within * 2] {
        for now in [
            Duration::ZERO,
            within / 3,
            within - nanosecond,
            within,
            within * 2,
        ] {
            assert_eq!(
                watch(absolute, started, started + moved, started + now),
                (now < within).then(|| within - now),
                "moved at {moved:?}, looked at {now:?}"
            );
        }
    }
    let every_instant = |at: Duration| at.as_nanos() as u64;
    assert_eq!(scripted(absolute, every_instant, None), (false, within));
    for (done, kept) in [(within - nanosecond, true), (within, false)] {
        assert_eq!(
            scripted(absolute, every_instant, Some((done, every_instant(done)))).0,
            kept,
            "finished at {done:?}"
        );
    }
}

/// With a window shorter than its guard, a silent exchange finishes in time only within a
/// window of its start, a moving one within a window of its last move, and no exchange past its
/// guard. NEGATIVE CONTROL, observed: without the guard (`watch` expiring only a window after the
/// last move), one that moved a nanosecond before its guard is not cut at it.
#[test]
fn a_hang_detector_judges_a_finish_by_the_last_move_and_the_guard() {
    let window = Duration::from_secs(60);
    let patience = Patience {
        window,
        guard: window * 10,
    };
    let s = Instant::now();
    let nanosecond = Duration::from_nanos(1);
    assert!(watch(patience, s, s, s + window - nanosecond).is_some());
    assert_eq!(watch(patience, s, s, s + window), None);
    assert!(watch(patience, s, s + window * 5, s + window * 5 + window / 2).is_some());
    assert_eq!(watch(patience, s, s + window * 5, s + window * 6), None);
    assert_eq!(
        watch(patience, s, s + window * 10 - nanosecond, s + window * 10),
        None
    );
    assert_eq!(
        watch(patience, s, s, s),
        Some(window / 4),
        "a hang detector looks four times a window"
    );
    let silent = |_: Duration| 0;
    for (done, kept) in [(window - nanosecond, true), (window, false)] {
        assert_eq!(
            scripted(patience, silent, Some((done, 0))).0,
            kept,
            "a silent exchange finished at {done:?}"
        );
    }
}

/// A progressing stream counts each read and write that moved a byte, and no other call.
/// NEGATIVE CONTROL, observed: counting every call that succeeded counts the empty read at its
/// end and the empty write: `` left: 4 right: 2 ``.
#[test]
fn a_progressing_stream_counts_the_calls_that_move_a_byte() {
    let moves = AtomicU64::new(0);
    let mut inner = std::io::Cursor::new(b"ab".to_vec());
    let mut stream = Progressing {
        stream: &mut inner,
        moves: &moves,
    };
    let mut bytes = [0; 4];
    assert_eq!(stream.read(&mut bytes).unwrap(), 2);
    assert_eq!(stream.read(&mut bytes).unwrap(), 0);
    assert_eq!(stream.write(b"c").unwrap(), 1);
    assert_eq!(stream.write(b"").unwrap(), 0);
    stream.flush().unwrap();
    assert_eq!(moves.load(Ordering::Relaxed), 2);
}

struct DeferredPeer {
    answers: std::io::Cursor<Vec<u8>>,
    written: Vec<u8>,
}
impl Read for DeferredPeer {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.answers.read(bytes)
    }
}
impl Write for DeferredPeer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.written.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn refusing(ids: impl IntoIterator<Item = Digest>) -> DeferredPeer {
    let mut answers = Vec::new();
    for id in ids {
        answers.extend_from_slice(&id);
        answers.push(2);
    }
    DeferredPeer {
        answers: std::io::Cursor::new(answers),
        written: Vec::new(),
    }
}
#[test]
fn repair_sweeps_past_a_deferred_page_after_retry_hints_expire() {
    let root = crate::tests::tmp();
    let dir = root.path().join("db");
    crate::local::init(&dir, "test").unwrap();
    let mut db = Database::open(&dir).unwrap();
    for n in 0..=(RECORDS_PER_ROUND as u128) {
        db.commit(Command::Write {
            request: n.to_le_bytes(),
            changes: vec![Change {
                collection: "example".into(),
                key: format!("r{n}"),
                value: Some(json!(n as u64)),
            }],
            said: None,
        })
        .unwrap();
    }
    let ids: Vec<_> = db.history.index().inventory().copied().collect();
    drop(db);
    let now = Instant::now();
    let actor = [1; 48];
    let mut repair = Repair::default();
    repair.prepare_at(actor, now);
    let mut first = refusing(ids[..RECORDS_PER_ROUND].iter().copied());
    assert_eq!(
        send_operations(&mut first, &dir, &BTreeSet::new(), &mut repair).unwrap(),
        (0, RECORDS_PER_ROUND)
    );
    repair.prepare_at(actor, now + Duration::from_secs(61));
    assert!(repair.deferred.is_empty());
    let expected = ids[RECORDS_PER_ROUND..]
        .iter()
        .chain(ids[..RECORDS_PER_ROUND - 1].iter())
        .copied();
    let mut second = refusing(expected);
    send_operations(&mut second, &dir, &BTreeSet::new(), &mut repair).unwrap();
    let frame_len = u32::from_le_bytes(second.written[4..8].try_into().unwrap()) as usize;
    assert_eq!(
        aterm_digest::Sha256::digest(&second.written[8..8 + frame_len]),
        ids[RECORDS_PER_ROUND],
        "a deferred prefix cannot starve the record after it"
    );
}

/// A bounded listener repairs both replicas, and the dialed round counts its stream's bytes as
/// its progress (`sync_round_with`'s deadline, [`LAST_MOVES`]), so a test's hang detector cuts it
/// only once it stops moving. NEGATIVE CONTROL, observed: with `sync_round_with` handing its
/// exchange `dialed.transport.stream()` uncounted, `` the dialed round's deadline counted Some(0)
/// moves ``.
#[test]
fn bounded_listener_repairs_both_replicas_and_stops_without_a_peer() {
    let (_root, a, b) = crate::tests::pair();
    for (dir, request, key) in [(&a, 1, "a"), (&b, 2, "b")] {
        Database::open(dir)
            .unwrap()
            .commit(Command::Write {
                request: [request; 16],
                changes: vec![Change {
                    collection: "example".into(),
                    key: key.into(),
                    value: Some(json!(request)),
                }],
                said: None,
            })
            .unwrap();
    }
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(Stop::new());
    let server = {
        let a = a.clone();
        let stop = stop.clone();
        thread::spawn(move || serve_until(&a, listener, stop, Arc::new(Mutex::new(Vec::new()))))
    };
    LAST_MOVES.with(std::cell::Cell::take);
    let first = sync_round(&b, address);
    let dialed = LAST_MOVES.with(std::cell::Cell::take);
    let second = first.as_ref().ok().map(|_| sync_round(&b, address));
    stop.set();
    server.join().unwrap().unwrap();
    let first = first.unwrap();
    assert_eq!((first.sent, first.received, first.deferred), (1, 1, 0));
    assert!(
        dialed.is_some_and(|moves| moves > 0),
        "the dialed round's deadline counted {dialed:?} moves"
    );
    assert_eq!(second.unwrap().unwrap(), Report::default());
    let inventory = |dir: &Path| {
        Database::open(dir)
            .unwrap()
            .history
            .index()
            .inventory()
            .copied()
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(inventory(&a), inventory(&b));
}

/// A served connection counts its stream's bytes as its progress (`serve_connection`'s deadline,
/// [`LAST_MOVES`] on the serving thread). NEGATIVE CONTROL, observed: with `serve_connection`
/// handing its exchange `accepted.transport.stream()` uncounted, `` the served connection's
/// deadline counted Some(0) moves ``.
#[test]
fn an_authenticated_peer_waiting_for_its_next_frame_does_not_hold_the_writer_lock() {
    let (_root, a, b) = crate::tests::pair();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let served = a.clone();
    let server = thread::spawn(move || {
        let connection = Connection::admit(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0,
            Scope::Loopback,
        )
        .unwrap();
        let report = serve_connection(&served, connection).unwrap();
        (report, LAST_MOVES.with(std::cell::Cell::take))
    });
    let credentials = Credentials::load(&b).unwrap();
    let (secret, identity) = transport(&credentials).unwrap();
    let tcp = TcpStream::connect(address).unwrap();
    let deadline = Deadline::arm(&tcp).unwrap();
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .unwrap();
    let stream = &mut deadline.progressing(dialed.transport.stream());
    send_frame(stream, credentials.capability.bytes()).unwrap();
    let certificate = receive_frame(stream, 256).unwrap();
    authenticate(&credentials, dialed.peer.as_ref(), &certificate).unwrap();
    send_inventory(stream, &BTreeSet::new()).unwrap();
    assert!(receive_inventory(stream).unwrap().is_empty());
    Database::open(&a)
        .expect("a peer waiting on network IO must leave the local writer available")
        .commit(Command::Write {
            request: [1; 16],
            changes: vec![Change {
                collection: "example".into(),
                key: "local".into(),
                value: Some(json!("works")),
            }],
            said: None,
        })
        .unwrap();
    stream.write_all(&0u32.to_le_bytes()).unwrap();
    stream.flush().unwrap();
    assert_eq!(receive_operations(stream, &b).unwrap(), (1, 0));
    let (report, served) = server.join().unwrap();
    assert_eq!(report.sent, 1);
    assert!(
        served.is_some_and(|moves| moves > 0),
        "the served connection's deadline counted {served:?} moves"
    );
}

#[test]
fn a_refused_frame_is_not_a_durable_ack_and_does_not_stop_the_next_operation() {
    let (_root, a, b) = crate::tests::pair();
    Database::open(&b)
        .unwrap()
        .commit(Command::Write {
            request: [1; 16],
            changes: vec![Change {
                collection: "example".into(),
                key: "valid".into(),
                value: Some(json!(true)),
            }],
            said: None,
        })
        .unwrap();
    let wire = crate::tests::wires(&b).pop().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let served = a.clone();
    let server = thread::spawn(move || {
        let connection = Connection::admit(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0,
            Scope::Loopback,
        )
        .unwrap();
        serve_connection(&served, connection).unwrap()
    });
    let credentials = Credentials::load(&b).unwrap();
    let (secret, identity) = transport(&credentials).unwrap();
    let tcp = TcpStream::connect(address).unwrap();
    let deadline = Deadline::arm(&tcp).unwrap();
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .unwrap();
    let stream = &mut deadline.progressing(dialed.transport.stream());
    send_frame(stream, credentials.capability.bytes()).unwrap();
    receive_frame(stream, 256).unwrap();
    send_inventory(
        stream,
        &BTreeSet::from([aterm_digest::Sha256::digest(&wire)]),
    )
    .unwrap();
    receive_inventory(stream).unwrap();
    stream.write_all(&2u32.to_le_bytes()).unwrap();
    for (bytes, expected) in [(&[0u8; 32][..], 2u8), (wire.as_slice(), 0)] {
        send_frame(stream, bytes).unwrap();
        let mut digest = [0; 32];
        stream.read_exact(&mut digest).unwrap();
        assert_eq!(digest, aterm_digest::Sha256::digest(bytes));
        let mut verdict = [0];
        stream.read_exact(&mut verdict).unwrap();
        assert_eq!(verdict, [expected]);
    }
    assert_eq!(receive_operations(stream, &b).unwrap(), (0, 0));
    let report = server.join().unwrap();
    assert_eq!(report.received, 1);
    assert_eq!(report.deferred, 1);
    assert_eq!(
        Database::open(&a)
            .unwrap()
            .history
            .index()
            .inventory()
            .count(),
        1
    );
}
