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
    let deadline = Deadline::arm_with_clock(&tcp, budget, move || {
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
        let deadline = Deadline::arm_with_clock(&tcp, budget, move || {
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
        deadline.done.send(started + completed_after).unwrap();
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
    let first = sync_round(&b, address);
    let second = first.as_ref().ok().map(|_| sync_round(&b, address));
    stop.set();
    server.join().unwrap().unwrap();
    let first = first.unwrap();
    assert_eq!((first.sent, first.received, first.deferred), (1, 1, 0));
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
        serve_connection(&served, connection).unwrap()
    });
    let credentials = Credentials::load(&b).unwrap();
    let (secret, identity) = transport(&credentials).unwrap();
    let tcp = TcpStream::connect(address).unwrap();
    let _deadline = Deadline::arm(&tcp).unwrap();
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .unwrap();
    let stream = dialed.transport.stream();
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
        })
        .unwrap();
    stream.write_all(&0u32.to_le_bytes()).unwrap();
    stream.flush().unwrap();
    assert_eq!(receive_operations(stream, &b).unwrap(), (1, 0));
    assert_eq!(server.join().unwrap().sent, 1);
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
    let _deadline = Deadline::arm(&tcp).unwrap();
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .unwrap();
    let stream = dialed.transport.stream();
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
