// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{
    age::Identity,
    commons::{
        binding::Request,
        constraints::Fact,
        hearing::Version,
        journal::{Journal, Trust},
        node::{Edit, Node},
    },
};

struct Lab {
    _root: aterm_tempfile::TempDir,
    dirs: [std::path::PathBuf; 2],
    trust: Trust,
    endpoints: [Arc<Endpoint>; 2],
    recipient: aterm_secrets::age::Recipient,
    genesis: Node,
    update: Node,
}
impl Lab {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let seed = [21; 32];
        let authority = crate::crypto::public(&seed).unwrap();
        let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
        let requests: [Request; 2] = std::array::from_fn(|n| {
            Request::create(
                [9; 16],
                authority,
                [n as u8 + 1; 16],
                [n as u8 + 3; 16],
                &identities[n],
                &[n as u8 + 31; 32],
            )
            .unwrap()
        });
        let facts: Vec<_> = requests
            .iter()
            .map(|r| Fact::Bind {
                binding: r.binding().clone(),
            })
            .collect();
        let genesis = Node::test_seal(
            &seed,
            [9; 16],
            Edit::Create {
                hosts: &[],
                payload: b"sealed",
            },
            &[identities[0].recipient()],
            0,
            &facts,
        )
        .unwrap();
        let update = Node::test_seal(
            &seed,
            [9; 16],
            Edit::Update {
                parent: genesis.id(),
                hosts: &[],
                payload: b"new state",
            },
            &[identities[0].recipient()],
            0,
            &[],
        )
        .unwrap();
        let trust = Trust {
            network: [9; 16],
            authority,
            genesis: genesis.id(),
        };
        let dirs = std::array::from_fn(|n| root.path().join(n.to_string()));
        let endpoints = std::array::from_fn(|n| {
            Arc::new(
                Endpoint::new(
                    &dirs[n],
                    trust,
                    &requests[n],
                    genesis.bytes(),
                    &[n as u8 + 31; 32],
                )
                .unwrap(),
            )
        });
        Journal::open_existing(&dirs[0], trust)
            .unwrap()
            .ingest(genesis.bytes())
            .unwrap();
        Journal::open_existing(&dirs[0], trust)
            .unwrap()
            .ingest(update.bytes())
            .unwrap();
        Self {
            _root: root,
            dirs,
            trust,
            endpoints,
            recipient: identities[0].recipient(),
            genesis,
            update,
        }
    }
}

fn accept_ready(listener: &Listener) -> Connection {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        match listener.accept() {
            Ok(connection) => return connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < until => {
                aterm_uds::deadline::poll_yield();
            }
            Err(error) => panic!("{error}"),
        }
    }
}

/// Waits until a connection waits in `listener`'s queue, a hang detector of
/// [`aterm_spec::wait::MUST_HAPPEN`]. A loopback `connect` can return before the kernel queues
/// the connection at its listener, and a tick tries each listener's accept once, without
/// blocking, so a test that judges what one tick accepts ticks only once the connection is
/// queued.
fn queued(listener: &Listener) {
    assert!(
        listener
            .pending_within(aterm_spec::wait::MUST_HAPPEN)
            .unwrap(),
        "a connection to {} reaches its queue",
        listener.local_addr().unwrap()
    );
}

#[test]
fn one_discovered_address_repairs_both_directions_and_cancellation_drains() {
    let lab = Lab::new();
    let listener =
        Arc::new(Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap());
    let addr = listener.local_addr().unwrap();
    let client = Worker::start(
        lab.endpoints[1].clone(),
        None,
        Some(addr),
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    let connection = accept_ready(&listener);
    let server = Worker::start(
        lab.endpoints[0].clone(),
        Some(connection),
        None,
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    let (_, repair, success, round) = client.task.join().unwrap();
    assert!(success);
    let (actor, endpoint) = round.authenticated_endpoint().unwrap();
    assert_eq!(Some(actor), repair.authenticated_actor());
    assert_eq!(endpoint.address, addr);
    assert_eq!(endpoint.observed_at_ms, round.completed_at_ms);
    let outbound = round.connection.unwrap();
    assert_eq!(outbound.peer, addr);
    assert!(outbound.began_ms > 0);
    assert!(outbound.authenticated_ms >= outbound.began_ms);
    assert!(outbound.authenticated_ms <= round.completed_at_ms);
    let (_, _, success, round) = server.task.join().unwrap();
    assert!(success);
    let inbound = round.connection.unwrap();
    assert_eq!(inbound.local, outbound.peer);
    assert_eq!(inbound.peer, outbound.local);
    assert!(inbound.began_ms > 0);
    assert!(inbound.authenticated_ms >= inbound.began_ms);
    assert!(inbound.authenticated_ms <= round.completed_at_ms);
    assert!(
        round.authenticated_endpoint().is_none(),
        "inbound source is not a service endpoint"
    );
    let journal = Journal::open_existing(&lab.dirs[1], lab.trust).unwrap();
    assert!(journal.node(&lab.genesis.id()).is_some());
    assert!(journal.node(&lab.update.id()).is_some());
    drop(journal);

    // A connected peer which never speaks must be interrupted by generation
    // teardown, before ownership is released. No endpoint timeout is awaited.
    let peer = std::net::TcpStream::connect(addr).unwrap();
    let connection = accept_ready(&listener);
    let worker = Worker::start(
        lab.endpoints[0].clone(),
        Some(connection),
        None,
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    let transport = Transport {
        endpoint: Some(lab.endpoints[0].clone()),
        links: vec![],
        hints: Hints::new([1; 32]),
        workers: vec![worker],
        beacon: Instant::now(),
        dial: Instant::now(),
        rotate: Instant::now() + Duration::from_secs(300),
        versions_at: Instant::now(),
        listened: false,
        held: aterm_net::fleet_member::HeldSignatures::default(),
        bus: Bus::default(),
        scope: Scope::Loopback,
        port: 0,
        contacts: Vec::new(),
        completed: VecDeque::new(),
        push: None,
        failure: None,
        doorbell: None,
        announced: Arc::default(),
        stand_in: None,
        delivering: None,
        observed: std::collections::BTreeMap::new(),
        finished_rounds: 0,
        memory: None,
    };
    let control = transport.workers[0].channel.clone();
    // Whether stop wins before install or during TLS, it cannot be undone.
    drop(transport);
    assert!(control.lock().unwrap().stopped);
    peer.set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut peer = peer;
    let mut byte = [0];
    use std::io::Read;
    match peer.read(&mut byte) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ) => {}
        result => panic!("drained connection did not close: {result:?}"),
    }
}

#[test]
fn transferred_control_history_keeps_a_follow_up_then_yields_when_caught_up() {
    let lab = Lab::new();
    let mut parent = lab.update.id();
    {
        let mut source = Journal::open_existing(&lab.dirs[0], lab.trust).unwrap();
        for n in 0..crate::control::RECORDS_PER_ROUND {
            let update = Node::test_seal(
                &[21; 32],
                [9; 16],
                Edit::Update {
                    parent,
                    hosts: &[],
                    payload: b"control backlog",
                },
                std::slice::from_ref(&lab.recipient),
                n as u64 + 1,
                &[],
            )
            .unwrap();
            source.ingest(update.bytes()).unwrap();
            parent = update.id();
        }
    }
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let address = listener.local_addr().unwrap();
    let other = SocketAddr::from(([127, 0, 0, 2], address.port()));
    let mut transport = Transport::unopened(None, Scope::Loopback, 0).unwrap();
    let mut now = Instant::now();
    for (index, peer) in [address, other].into_iter().enumerate() {
        transport.hints.observe(
            &Hints::new([index as u8 + 71; 32]).wire(peer.port()),
            peer,
            now,
        );
    }
    let exchange = |repair| {
        let client = Worker::start(
            lab.endpoints[1].clone(),
            None,
            Some(address),
            Scope::Loopback,
            None,
            repair,
            Instant::now(),
        )
        .unwrap();
        let server = Worker::start(
            lab.endpoints[0].clone(),
            Some(accept_ready(&listener)),
            None,
            Scope::Loopback,
            None,
            Repair::default(),
            Instant::now(),
        )
        .unwrap();
        assert!(server.task.join().unwrap().2);
        client
    };

    for received in [crate::control::RECORDS_PER_ROUND, 2, 0] {
        let (chosen, repair) = transport.hints.next(now).unwrap();
        assert_eq!(chosen, address, "control progress lost its follow-up");
        transport.finish(exchange(repair), now);
        let round = transport.reports().next().unwrap();
        assert!(round.membership.is_none());
        assert!(round.application.is_none());
        assert_eq!(round.control.unwrap().received, received);
        if received != 0 {
            // Advance a virtual scheduler clock past the short follow-up jitter,
            // still before the ordinary interval. No wall-clock latency budget.
            now += Duration::from_millis(500);
        }
    }
    assert_eq!(
        transport.hints.next(now).unwrap().0,
        other,
        "a caught-up control peer must yield to ordinary discovery"
    );
    let inventory = |index: usize| {
        Journal::open_existing(lab.dirs[index].as_path(), lab.trust)
            .unwrap()
            .inventory()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let expected = inventory(0);
    assert_eq!(expected.len(), crate::control::RECORDS_PER_ROUND + 2);
    assert_eq!(inventory(1), expected);
}

#[test]
fn cancellation_before_socket_install_cannot_be_reversed() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let connection = Connection::connect(
        listener.local_addr().unwrap(),
        Scope::Loopback,
        Duration::from_secs(3),
    )
    .unwrap();
    let mut channel = Channel::default();
    channel.stop();
    assert!(!channel.install(&connection, 1).unwrap());
    assert!(channel.socket.is_none());
}

/// One real round: `client` dials `listener`, whose side `server` serves; the dialling worker,
/// then the serving one.
fn round_at(
    client: &Arc<Endpoint>,
    server: &Arc<Endpoint>,
    listener: &Listener,
    repair: Repair,
) -> (Worker, Worker) {
    let address = listener.local_addr().unwrap();
    let dialled = Worker::start(
        client.clone(),
        None,
        Some(address),
        Scope::Loopback,
        None,
        repair,
        Instant::now(),
    )
    .unwrap();
    let served = Worker::start(
        server.clone(),
        Some(accept_ready(listener)),
        None,
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    (dialled, served)
}

/// The seats read ([`Transport::hear_versions`], which a transport's tick runs every [`SEATS`])
/// checks an unchanged 85-entry fleet book's signatures once: the transport keeps each verdict,
/// so its second read checks none, and a read after one entry changed checks that entry alone.
/// Counted on this thread ([`census::cadence_here`]).
///
/// NEGATIVE CONTROL (2026-11-12): with `HeldSignatures::judge` never looking a kept verdict up,
/// the second read made 85 checks (`the second read checks nothing`: left 85, right 0).
/// NEGATIVE CONTROL (2026-11-12): with a verdict kept under the node id and key alone, the
/// changed entry was not checked (`a changed entry is checked alone`: left 0, right 1).
#[test]
fn the_seats_read_checks_an_unchanged_books_signatures_once() {
    let producer = crate::control::advertised_tests::Producer::new();
    let book = producer.crowd();
    producer.keep_all(&book);
    let mut changed = book.clone();
    changed[0].version = Some("2.0.0".into());
    let crate::control::advertised_tests::Producer {
        lab: _lab,
        active,
        state,
        ..
    } = producer;
    let mut transport = Transport::unopened(None, Scope::Loopback, 0).unwrap();
    transport.endpoint = Some(Arc::new(active));
    let now = Instant::now();
    let read = |transport: &mut Transport| {
        let before = census::cadence_here().signature_checks;
        transport.hear_versions(&state, now);
        census::cadence_here().signature_checks - before
    };
    assert_eq!(
        read(&mut transport),
        85,
        "the first read checks every entry"
    );
    assert_eq!(read(&mut transport), 0, "the second read checks nothing");
    aterm_net::fleet_member::update_peers(&state, |held| {
        held.clear();
        held.extend(changed.iter().cloned());
        Ok::<(), String>(())
    })
    .unwrap();
    assert_eq!(read(&mut transport), 1, "a changed entry is checked alone");
    assert_eq!(read(&mut transport), 0, "and its verdict kept");
}

/// Send `datagrams` to `transport`'s first link's multicast socket from one loopback socket, in
/// order, and return once every one of them waits there. The loopback interface delivers what
/// is sent through it in the order sent, whichever socket each datagram is for, so the test
/// sends one more to a socket of its own and waits for that one: a hang detector, never a
/// budget.
fn queue(transport: &Transport, datagrams: &[&[u8]]) {
    let sender = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = transport.links[0].hints.port();
    for datagram in datagrams {
        sender
            .send_to(datagram, (Ipv4Addr::LOCALHOST, port))
            .unwrap();
    }
    let last = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    last.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    sender.send_to(b"last", last.local_addr().unwrap()).unwrap();
    let mut bytes = [0; 8];
    assert_eq!(
        last.recv(&mut bytes).unwrap(),
        4,
        "the loopback delivered what was sent before it"
    );
}

/// `transport`'s turn at `at`, with how many beacons it read ([`census::Cadence::beacons`]).
fn beacons_read(transport: &mut Transport, at: Instant, hear: bool) -> u64 {
    let before = census::cadence_here().beacons;
    transport.turn(at, hear).unwrap();
    census::cadence_here().beacons - before
}

/// A transport past its first turn whose own beacon is not due before `now + BEACON`: its turns
/// hear only when asked.
fn settled(now: Instant) -> Transport {
    let mut transport = loopback_mode(None, 0, [91; 32]);
    transport.listened = true;
    transport.beacon = now + BEACON;
    transport
}

/// The owner loop waits on its doorbell and on each listener that has room, and on no multicast
/// socket, however many links it has ([`Transport::waits_on`]): a beacon wakes nothing.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Transport::waits_on`] listing each link's multicast
/// socket again, one link gave three descriptors (`1 link(s)`: left Some(3), right Some(2)).
#[test]
fn the_owner_waits_on_its_doorbell_and_listeners_and_no_multicast_socket() {
    let lab = Lab::new();
    let mut transport = Transport::unopened(None, Scope::Loopback, 0).unwrap();
    transport.endpoint = Some(lab.endpoints[0].clone());
    let link = |listener: bool| Link {
        source: Ipv4Addr::LOCALHOST.into(),
        listener: listener
            .then(|| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap()),
        hints: Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap(),
        delivering: None,
    };
    let waits = |transport: &Transport| transport.waits_on().map(|fds| fds.len());
    assert_eq!(waits(&transport), Some(1), "its doorbell");
    for links in 1..=3 {
        transport.links.push(link(true));
        assert_eq!(waits(&transport), Some(1 + links), "{links} link(s)");
    }
    transport.links.push(link(false));
    assert_eq!(
        waits(&transport),
        Some(4),
        "a link with no listener adds nothing to wait on"
    );
    // A beacon that waits in a link's socket leaves the loop's wait unready.
    queue(&transport, &[&Hints::new([1; 32]).wire(4001)]);
    assert!(
        aterm_uds::ready::any_now(&transport.waits_on().unwrap())
            .unwrap()
            .is_empty(),
        "a waiting beacon wakes nothing"
    );
}

/// Beacons wait in their socket until a hearing turn, which reads every one of them: with 40
/// queued, three turns that were not asked to hear read none, the turn that was reads all 40,
/// and the next finds none left. A transport's first turn hears unasked, and so does the turn
/// its own beacon is due.
///
/// NEGATIVE CONTROL (2026-11-12): with the read's bound back at 16 a turn, the hearing turn read
/// 16 (`a hearing turn reads every beacon that waits`: left 16, right 40).
#[test]
fn a_hearing_turn_reads_every_queued_beacon_and_no_other_turn_reads_one() {
    use crate::runtime::TICK;
    let now = Instant::now();
    let mut transport = settled(now);
    let wires: Vec<_> = (0..40u8)
        .map(|n| Hints::new([n + 1; 32]).wire(4000 + u16::from(n)))
        .collect();
    let datagrams: Vec<&[u8]> = wires.iter().map(|wire| wire.as_slice()).collect();
    queue(&transport, &datagrams);
    for tick in 1..=3 {
        assert_eq!(
            beacons_read(&mut transport, now + TICK * tick, false),
            0,
            "a turn not asked to hear reads none"
        );
    }
    assert_eq!(
        beacons_read(&mut transport, now + TICK * 4, true),
        40,
        "a hearing turn reads every beacon that waits"
    );
    assert_eq!(beacons_read(&mut transport, now + TICK * 5, true), 0);

    // A transport's first turn hears, asked or not.
    let mut first = settled(now);
    first.listened = false;
    queue(&first, &datagrams[..1]);
    assert_eq!(beacons_read(&mut first, now, false), 1, "the first turn");
    queue(&first, &datagrams[..1]);
    assert_eq!(beacons_read(&mut first, now + TICK, false), 0, "the second");
    // So does the turn its own beacon is due, which may read its own beacon back as well.
    assert!(
        beacons_read(&mut first, now + BEACON, false) >= 1,
        "the turn its own beacon is due"
    );
}

/// One hearing turn reads at most [`HEARD`] datagrams from a link, dropped ones included, and
/// leaves the rest in the socket for the next: with an oversized datagram and seven beacons
/// more than the bound queued, the first hearing turn reads the dropped one and `HEARD - 1`
/// beacons, the second the eight left, the third none. The bound is what an unauthenticated
/// flood can ask of one turn.
///
/// NEGATIVE CONTROL (2026-11-12): with the read going on until the socket is empty, the first
/// turn read all 327 beacons (`the bound's datagrams, then the rest, then none`: `left: [327,
/// 0, 0]`, `right: [319, 8, 0]`).
#[test]
fn a_hearing_turn_reads_no_more_than_its_bound_and_leaves_the_rest() {
    use crate::runtime::TICK;
    let now = Instant::now();
    let mut transport = settled(now);
    let wires: Vec<_> = (0..HEARD + 7)
        .map(|n| Hints::new([(n % 250) as u8 + 1; 32]).wire(4000 + n as u16))
        .collect();
    // Larger than a discovery datagram may be (`aterm_net::lan::Multicast::hear`).
    let oversized = [0; 1300];
    let mut datagrams: Vec<&[u8]> = vec![&oversized];
    datagrams.extend(wires.iter().map(|wire| wire.as_slice()));
    queue(&transport, &datagrams);
    let read: Vec<u64> = (1..=3)
        .map(|turn| beacons_read(&mut transport, now + TICK * turn, true))
        .collect();
    assert_eq!(
        read,
        [HEARD as u64 - 1, 8, 0],
        "the bound's datagrams, then the rest, then none"
    );
}

/// The serving loops' polled round is the turn that hears. [`crate::runtime::serve_paced`] runs
/// a real transport's step ([`crate::runtime::serve_turn`], what `serve_selected` and
/// `serve_presence` run in their ticks) on a virtual clock, a tick every 500 ms for five
/// seconds. A beacon that waits from before the first tick is read by the tick of the first
/// round, two seconds in, and by none before it; one that comes after that round is read by
/// the tick of the next, and by none between: 0, 0, 0, 0, 1, 0, 0, 0, 1, 0 read by the ten
/// ticks. A beacon so waits a polled round at most.
///
/// NEGATIVE CONTROL (2026-11-12): with `serve_turn` asking no turn to hear, neither round's
/// tick read its beacon (`each tick's (polled round, beacons read)`: `left` reads `(true, 0)`
/// for both rounds where `right` reads `(true, 1)`).
#[test]
fn the_serving_loops_polled_round_is_the_turn_that_hears() {
    use crate::runtime::{Flow, Status, serve_paced, serve_turn};
    let start = Instant::now();
    let clock = std::cell::Cell::new(start);
    let mut transport = settled(start);
    queue(&transport, &[&Hints::new([1; 32]).wire(4001)]);
    let mut read = Vec::new();
    let served: std::result::Result<(), Status> = serve_paced(
        &mut transport,
        || clock.get(),
        || clock.get() >= start + BEACON,
        |transport, floor, _| {
            let now = clock.get();
            if now == start + crate::runtime::POLL {
                queue(transport, &[&Hints::new([2; 32]).wire(4002)]);
            }
            clock.set((now + Duration::from_millis(500)).max(floor));
        },
        |transport, now, round| {
            let before = census::cadence_here().beacons;
            serve_turn(transport, now, round)?;
            read.push((round, census::cadence_here().beacons - before));
            Ok(Flow::Serve(Some(transport.next_due(now))))
        },
    );
    assert_eq!(served, Ok(()));
    let (idle, heard) = ((false, 0), (true, 1));
    assert_eq!(
        read,
        [idle, idle, idle, idle, heard, idle, idle, idle, heard, idle],
        "each tick's (polled round, beacons read)"
    );
}

/// A datagram the socket drops ends no read: with a beacon, an oversized datagram and a second
/// beacon queued, one hearing turn reads both beacons and admits both endpoints.
///
/// NEGATIVE CONTROL (2026-11-12): with a dropped datagram read as an empty socket, the turn read
/// one beacon (`both beacons, past the dropped datagram`: left 1, right 2).
#[test]
fn a_dropped_datagram_does_not_end_a_hearing_turns_read() {
    use crate::runtime::TICK;
    let now = Instant::now();
    let mut transport = settled(now);
    let (first, second) = (
        Hints::new([1; 32]).wire(4001),
        Hints::new([2; 32]).wire(4002),
    );
    // Larger than a discovery datagram may be (`aterm_net::lan::Multicast::hear`).
    let oversized = [0; 1300];
    queue(&transport, &[&first, &oversized, &second]);
    assert_eq!(
        beacons_read(&mut transport, now + TICK, true),
        2,
        "both beacons, past the dropped datagram"
    );
    assert_eq!(transport.peers(), 2, "both endpoints are candidates");
}

/// Each round's end adds one to exactly one of the outbound and inbound counts
/// ([`census::Cadence`]), by who dialled it and whatever it came to, and a round that completed
/// and moved nothing adds one to the quiet count as well. Read on this thread, which takes each
/// end ([`census::cadence_here`]).
///
/// NEGATIVE CONTROL (2026-11-12): with [`Transport::finish`] counting every end as dialled in as
/// well as by its direction, the first end read `[1, 1, 0]` where `[1, 0, 0]` was due (`the
/// dialling side's end is one outbound round`).
#[test]
fn a_rounds_end_counts_once_by_who_dialled_it() {
    let lab = Lab::new();
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let mut dialler = loopback_mode(None, 0, [71; 32]);
    let mut server = loopback_mode(None, 0, [72; 32]);
    let now = Instant::now();
    let read = || {
        let counts = census::cadence_here();
        [counts.rounds_out, counts.rounds_in, counts.rounds_quiet]
    };
    let start = read();
    let since = || {
        let counts = read();
        [0, 1, 2].map(|at| counts[at] - start[at])
    };
    // The first round carries the control history across: neither end is quiet.
    let (dialled, served) = round_at(
        &lab.endpoints[1],
        &lab.endpoints[0],
        &listener,
        Repair::default(),
    );
    dialler.finish(dialled, now);
    assert_eq!(
        since(),
        [1, 0, 0],
        "the dialling side's end is one outbound round"
    );
    server.finish(served, now);
    assert_eq!(
        since(),
        [1, 1, 0],
        "the serving side's end is one inbound round"
    );
    // The second finds nothing to move: each side's end is quiet.
    let (dialled, served) = round_at(
        &lab.endpoints[1],
        &lab.endpoints[0],
        &listener,
        Repair::default(),
    );
    dialler.finish(dialled, now);
    assert_eq!(since(), [2, 1, 1], "a quiet outbound round");
    server.finish(served, now);
    assert_eq!(since(), [2, 2, 2], "a quiet inbound round");
    // A dial nothing answers ends as an outbound round, and is not quiet.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target = closed.local_addr().unwrap();
    drop(closed);
    let refused = Worker::start(
        lab.endpoints[1].clone(),
        None,
        Some(target),
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    dialler.finish(refused, now);
    assert_eq!(since(), [3, 2, 2], "a refused dial is an outbound round");
}

/// A peer whose endpoint here keeps refusing (it came back on another port) dials in and completes
/// a round: its endpoints here drop their backoff, as one this side dialled would (CR1). Once per
/// failure streak: when the endpoint it pulled in refuses again, it is dead, and the peer's later
/// rounds leave its backoff to grow.
/// MUTANT (`UNVOUCHED`, as before CR1): the refused endpoint keeps its backoff.
/// MUTANT (`REPULLS`, CR1's first cut): every round from the peer pulls the dead endpoint in.
#[test]
fn an_inbound_round_from_a_peer_ends_its_endpoints_backoff() {
    for unvouched in [false, true] {
        let lab = Lab::new();
        let mut a = loopback_mode(None, 0, [71; 32]);
        let old = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let ours = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let address = old.local_addr().unwrap();
        let mut now = Instant::now();
        a.hints
            .observe(&Hints::new([72; 32]).wire(address.port()), address, now);
        // A dials B until a round moves nothing: B's endpoint here is verified.
        for _ in 0..3 {
            let (chosen, repair) = a.hints.next(now).unwrap();
            assert_eq!(chosen, address);
            let (dialled, served) = round_at(&lab.endpoints[0], &lab.endpoints[1], &old, repair);
            assert!(served.task.join().unwrap().2);
            a.finish(dialled, now);
            now += Duration::from_millis(500);
            if !a.hints.follow_up_contains(address) {
                break;
            }
        }
        assert!(a.hints.established(address));
        // B goes away and comes back on another port: its old one refuses each dial.
        drop(old);
        for _ in 0..3 {
            now = a.hints.next_at(address).max(now);
            let (chosen, repair) = a.hints.next(now).unwrap();
            assert_eq!(chosen, address);
            let dialled = Worker::start(
                lab.endpoints[0].clone(),
                None,
                Some(address),
                Scope::Loopback,
                None,
                repair,
                Instant::now(),
            )
            .unwrap();
            a.finish(dialled, now);
        }
        assert!(a.hints.next_at(address) > now + Duration::from_secs(10));
        // B dials A and completes a round.
        let (dialled, served) = round_at(
            &lab.endpoints[1],
            &lab.endpoints[0],
            &ours,
            Repair::default(),
        );
        assert!(dialled.task.join().unwrap().2);
        UNVOUCHED.with(|flag| flag.set(unvouched));
        a.finish(served, now);
        UNVOUCHED.with(|flag| flag.set(false));
        let quiet = a.hints.next_at(address) <= now + Duration::from_secs(5);
        assert_eq!(quiet, !unvouched, "MUTANT {unvouched}: the backoff ended");
        if unvouched {
            continue;
        }
        // The endpoint pulled in refuses again, and B goes on dialling in.
        now = a.hints.next_at(address).max(now);
        let (chosen, repair) = a.hints.next(now).unwrap();
        assert_eq!(chosen, address);
        let dialled = Worker::start(
            lab.endpoints[0].clone(),
            None,
            Some(address),
            Scope::Loopback,
            None,
            repair,
            Instant::now(),
        )
        .unwrap();
        a.finish(dialled, now);
        for repulls in [false, true] {
            let (dialled, served) = round_at(
                &lab.endpoints[1],
                &lab.endpoints[0],
                &ours,
                Repair::default(),
            );
            assert!(dialled.task.join().unwrap().2);
            crate::runtime::discovery::REPULLS.with(|flag| flag.set(repulls));
            a.finish(served, now);
            crate::runtime::discovery::REPULLS.with(|flag| flag.set(false));
            // An ordinary interval is under 5 s; the first backoff step is 5 s or more.
            let backing_off = a.hints.next_at(address) >= now + Duration::from_secs(5);
            assert_eq!(
                backing_off, !repulls,
                "MUTANT {repulls}: pulled in once per streak"
            );
        }
    }
}

/// C4 through real rounds and [`Transport::finish`], which reads each round dialled in as
/// quiet or not. A's endpoint for B is verified and at rest. B dials A with nothing to move:
/// A's own dial of B waits an ordinary interval again from that round. B then takes a record A
/// lacks and dials again: that round moves it, and A's dial stays where it was.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::vouch`] not moving a resting endpoint's next
/// dial, A's dial stayed inside the interval of its own last round (`a quiet round dialled in
/// holds this side's dial back`).
/// NEGATIVE CONTROL (2026-11-12): with [`Transport::finish`] calling every round dialled in
/// quiet, the round that moved the record held A's dial back again (`a round that moved a
/// record holds nothing back`).
#[test]
fn a_quiet_round_dialled_in_holds_this_sides_dial_back_and_one_that_moved_a_record_does_not() {
    let lab = Lab::new();
    let mut a = loopback_mode(None, 0, [71; 32]);
    let theirs = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let ours = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let address = theirs.local_addr().unwrap();
    let mut now = Instant::now();
    a.hints
        .observe(&Hints::new([72; 32]).wire(address.port()), address, now);
    // A dials B until a round moves nothing: B's endpoint here is verified and at rest.
    for _ in 0..3 {
        let (chosen, repair) = a.hints.next(now).unwrap();
        assert_eq!(chosen, address);
        let (dialled, served) = round_at(&lab.endpoints[0], &lab.endpoints[1], &theirs, repair);
        assert!(served.task.join().unwrap().2);
        a.finish(dialled, now);
        if !a.hints.follow_up_contains(address) {
            break;
        }
        now += Duration::from_millis(500);
    }
    assert!(a.hints.established(address));
    let rested = a.hints.next_at(address);
    assert!(rested < now + Duration::from_secs(5), "its own interval");

    let dial_in = || {
        let (dialled, served) = round_at(
            &lab.endpoints[1],
            &lab.endpoints[0],
            &ours,
            Repair::default(),
        );
        assert!(dialled.task.join().unwrap().2);
        served
    };
    let heard = now + Duration::from_secs(2);
    a.finish(dial_in(), heard);
    let held = a.hints.next_at(address);
    assert!(
        held >= heard + Duration::from_secs(3),
        "a quiet round dialled in holds this side's dial back"
    );

    let after = Node::test_seal(
        &[21; 32],
        [9; 16],
        Edit::Update {
            parent: lab.update.id(),
            hosts: &[],
            payload: b"written at B",
        },
        &[Identity::generate().unwrap().recipient()],
        0,
        &[],
    )
    .unwrap();
    Journal::open_existing(&lab.dirs[1], lab.trust)
        .unwrap()
        .ingest(after.bytes())
        .unwrap();
    a.finish(dial_in(), heard + Duration::from_secs(2));
    assert!(
        Journal::open_existing(&lab.dirs[0], lab.trust)
            .unwrap()
            .node(&after.id())
            .is_some(),
        "the second round dialled in moved B's record to A"
    );
    assert_eq!(
        a.hints.next_at(address),
        held,
        "a round that moved a record holds nothing back"
    );
}

#[test]
fn repeated_listener_restarts_do_not_pin_failed_verified_discovery_ports() {
    let lab = Lab::new();
    let mut hints = Hints::new([61; 32]);
    // Bind all three first so the kernel cannot reuse an old listener's port.
    let listeners: Vec<_> = (0..3)
        .map(|_| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
        .collect();
    let mut now = Instant::now();
    let mut old_addresses = Vec::new();
    for listener in &listeners[..2] {
        let address = listener.local_addr().unwrap();
        hints.observe(&Hints::new([62; 32]).wire(address.port()), address, now);
        // Finish any real control follow-up before observing another listener.
        // The first round copies this fixture's two nodes; the next is empty.
        for turn in 0..2 {
            let (chosen, repair) = hints.next(now).unwrap();
            assert_eq!(chosen, address);
            let client = Worker::start(
                lab.endpoints[1].clone(),
                None,
                Some(address),
                Scope::Loopback,
                None,
                repair,
                Instant::now(),
            )
            .unwrap();
            let server = Worker::start(
                lab.endpoints[0].clone(),
                Some(accept_ready(listener)),
                None,
                Scope::Loopback,
                None,
                Repair::default(),
                Instant::now(),
            )
            .unwrap();
            let (_, repair, success, round) = client.task.join().unwrap();
            assert!(server.task.join().unwrap().2);
            assert!(success);
            let outcome = Outcome::of(success, &repair, round.control.ok());
            hints.finish(address, repair, outcome, now);
            if outcome != (Outcome::Repaired { transferred: true }) {
                break;
            }
            assert_eq!(turn, 0, "the caught-up control round must yield");
            now += Duration::from_millis(500);
        }
        old_addresses.push(address);
    }
    let mut listeners = listeners;
    let current = listeners.pop().unwrap();
    drop(listeners);
    // Observe real failed dials, not an untrusted beacon's claim of failure.
    let retry = now + Duration::from_secs(10);
    for _ in 0..2 {
        let (address, repair) = hints.next(retry).unwrap();
        assert!(old_addresses.contains(&address));
        let client = Worker::start(
            lab.endpoints[1].clone(),
            None,
            Some(address),
            Scope::Loopback,
            None,
            repair,
            Instant::now(),
        )
        .unwrap();
        let (_, repair, success, round) = client.task.join().unwrap();
        assert!(!success);
        let outcome = Outcome::of(success, &repair, round.control.ok());
        hints.finish(address, repair, outcome, retry);
    }
    let address = current.local_addr().unwrap();
    hints.observe(&Hints::new([63; 32]).wire(address.port()), address, retry);
    let (chosen, repair) = hints
        .next(retry)
        .expect("failed verified ports must not block the current listener for ten minutes");
    assert_eq!(chosen, address);
    let client = Worker::start(
        lab.endpoints[1].clone(),
        None,
        Some(address),
        Scope::Loopback,
        None,
        repair,
        Instant::now(),
    )
    .unwrap();
    let server = Worker::start(
        lab.endpoints[0].clone(),
        Some(accept_ready(&current)),
        None,
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    assert!(client.task.join().unwrap().2);
    assert!(server.task.join().unwrap().2);
}

fn loopback(endpoint: Arc<Endpoint>, port: u16, token: [u8; 32]) -> Transport {
    loopback_mode(Some(endpoint), port, token)
}

#[test]
fn incoming_acceptance_rotates_after_a_connection_not_idle_ticks() {
    let lab = Lab::new();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [91; 32]);
    let first = transport.links[0]
        .listener
        .as_ref()
        .unwrap()
        .local_addr()
        .unwrap();
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let second = listener.local_addr().unwrap();
    // Distinct loopback listeners stand in for two physical attachment entries.
    transport.links.push(Link {
        source: second.ip(),
        listener: Some(listener),
        hints: Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap(),
        delivering: None,
    });
    let mut held = vec![std::net::TcpStream::connect(first).unwrap()];
    queued(transport.links[0].listener.as_ref().unwrap());
    let now = Instant::now();
    transport.tick(now).unwrap();
    assert_eq!(transport.workers.len(), 1);
    // Polls with no newly accepted connection must not change who goes next.
    for tick in 1..=3 {
        transport
            .tick(now + Duration::from_millis(tick * 100))
            .unwrap();
        assert_eq!(transport.workers.len(), 1);
    }
    held.push(std::net::TcpStream::connect(first).unwrap());
    held.push(std::net::TcpStream::connect(second).unwrap());
    // Tick only once each attachment really holds one waiting.
    for link in &transport.links {
        queued(link.listener.as_ref().unwrap());
    }
    transport.tick(now + Duration::from_millis(400)).unwrap();
    let accepted: Vec<_> = transport
        .workers
        .iter()
        .map(|worker| {
            assert!(worker.incoming);
            worker.channel.lock().unwrap().local_endpoint.unwrap()
        })
        .collect();
    assert_eq!(
        accepted,
        [first, second],
        "a busy first attachment took the second attachment's incoming slot"
    );
    transport.stop();
    assert!(
        transport.take_contacts().is_empty(),
        "unproved connections grant no peer identity"
    );
    drop(held);
}

#[test]
fn stalled_outgoing_repairs_leave_room_for_an_incoming_peer() {
    let lab = Lab::new();
    let stalled: Vec<_> = (0..4)
        .map(|_| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
        .collect();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [11; 32]);
    let mut held = Vec::new();
    for listener in &stalled[..2] {
        transport.workers.push(
            Worker::start(
                lab.endpoints[0].clone(),
                None,
                Some(listener.local_addr().unwrap()),
                Scope::Loopback,
                None,
                Repair::default(),
                Instant::now(),
            )
            .unwrap(),
        );
        held.push(accept_ready(listener));
    }
    let now = Instant::now() + Duration::from_secs(1);
    for (index, listener) in stalled[2..].iter().enumerate() {
        let address = listener.local_addr().unwrap();
        transport.hints.observe(
            &Hints::new([index as u8 + 12; 32]).wire(address.port()),
            address,
            now,
        );
    }
    transport.tick(now).unwrap();
    transport.tick(now + Duration::from_secs(1)).unwrap();
    let connection = Connection::connect(
        transport.links[0]
            .listener
            .as_ref()
            .unwrap()
            .local_addr()
            .unwrap(),
        Scope::Loopback,
        Duration::from_secs(60),
    )
    .unwrap();
    let peer = lab.endpoints[1].clone();
    thread::scope(|tasks| {
        // On an assertion failure, drop the listener and cancel its workers
        // before the scope waits for the queued peer.
        let mut transport = transport;
        let incoming =
            tasks.spawn(move || peer.exchange(connection, Side::Initiator, &mut Repair::default()));
        queued(transport.links[0].listener.as_ref().unwrap());
        transport.tick(now + Duration::from_secs(2)).unwrap();
        assert!(
            transport.workers.iter().any(|worker| worker.incoming),
            "stalled outgoing repairs must leave capacity to accept a peer"
        );
        incoming.join().unwrap().unwrap();
        transport.stop();
    });
    drop(held);
    assert!(
        Journal::open_existing(&lab.dirs[1], lab.trust)
            .unwrap()
            .node(&lab.update.id())
            .is_some()
    );
}

#[test]
fn unproved_candidates_leave_an_outgoing_slot_for_an_established_peer() {
    let lab = Lab::new();
    let mut listeners: Vec<_> = (0..3)
        .map(|_| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
        .collect();
    listeners.sort_by_key(|listener| listener.local_addr().unwrap());
    let known = listeners.pop().unwrap();
    let address = known.local_addr().unwrap();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [47; 32]);
    let mut now = Instant::now();
    transport
        .hints
        .observe(&Hints::new([48; 32]).wire(address.port()), address, now);
    for turn in 0..2 {
        let (selected, repair) = transport.hints.next(now).unwrap();
        assert_eq!(selected, address);
        let client = Worker::start(
            lab.endpoints[0].clone(),
            None,
            Some(address),
            Scope::Loopback,
            None,
            repair,
            Instant::now(),
        )
        .unwrap();
        let server = Worker::start(
            lab.endpoints[1].clone(),
            Some(accept_ready(&known)),
            None,
            Scope::Loopback,
            None,
            Repair::default(),
            Instant::now(),
        )
        .unwrap();
        let (_, repair, success, report) = client.task.join().unwrap();
        assert!(success);
        assert!(server.task.join().unwrap().2);
        let outcome = Outcome::of(success, &repair, report.control.ok());
        if turn == 1 {
            assert_eq!(outcome, Outcome::Repaired { transferred: false });
        }
        transport.hints.finish(address, repair, outcome, now);
        now += Duration::from_secs(1);
    }
    // A new signed update arrives only after the peer has completed a real round.
    let update = Node::test_seal(
        &[21; 32],
        [9; 16],
        Edit::Update {
            parent: lab.update.id(),
            hosts: &[],
            payload: b"after the established round",
        },
        std::slice::from_ref(&lab.recipient),
        0,
        &[],
    )
    .unwrap();
    Journal::open_existing(&lab.dirs[0], lab.trust)
        .unwrap()
        .ingest(update.bytes())
        .unwrap();

    // One unanswered candidate already holds an outgoing slot. Keep every
    // real socket on portable 127.0.0.1; only the two queued peers need hints.
    transport.workers.push(
        Worker::start(
            lab.endpoints[0].clone(),
            None,
            Some(listeners[0].local_addr().unwrap()),
            Scope::Loopback,
            None,
            Repair::default(),
            Instant::now(),
        )
        .unwrap(),
    );
    let mut held = vec![accept_ready(&listeners[0])];
    let candidate = listeners[1].local_addr().unwrap();
    transport
        .hints
        .observe(&Hints::new([49; 32]).wire(candidate.port()), candidate, now);
    transport.dial = now;
    transport.tick(now + Duration::from_millis(300)).unwrap();
    if transport
        .workers
        .iter()
        .any(|worker| worker.target == Some(candidate))
    {
        held.push(accept_ready(&listeners[1]));
    }
    // Virtual time passes the normal retry. Neither silent TLS peer has been
    // released: an established peer must not wait for their network deadlines.
    transport.tick(now + Duration::from_secs(6)).unwrap();
    assert!(
        transport
            .workers
            .iter()
            .any(|worker| worker.target == Some(address)),
        "unproved candidates occupied the established peer's outgoing capacity"
    );
    let server = Worker::start(
        lab.endpoints[1].clone(),
        Some(accept_ready(&known)),
        None,
        Scope::Loopback,
        None,
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    assert!(server.task.join().unwrap().2);
    transport.stop();
    drop(held);
    assert!(
        Journal::open_existing(&lab.dirs[1], lab.trust)
            .unwrap()
            .node(&update.id())
            .is_some(),
        "the established peer did not retain the new signed update"
    );
}

#[test]
fn pending_enrollment_uses_all_workers_without_a_listener() {
    let lab = crate::control::enrollment::tests::Lab::new(true);
    let endpoint = Arc::new(lab.pending());
    let rounds = ROUNDS + INCOMING;
    let stalled: Vec<_> = (0..rounds)
        .map(|_| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
        .collect();
    let mut transport = loopback(endpoint.clone(), 0, [81; 32]);
    assert!(transport.links.iter().all(|link| link.listener.is_none()));
    let mut held = Vec::new();
    // Two dials a moment ago, within their stagger: they hold the two places a serving node has.
    let now = Instant::now() + Duration::from_secs(1);
    for listener in &stalled[..ROUNDS] {
        transport.workers.push(
            Worker::start(
                endpoint.clone(),
                None,
                Some(listener.local_addr().unwrap()),
                Scope::Loopback,
                None,
                Repair::default(),
                now,
            )
            .unwrap(),
        );
        held.push(accept_ready(listener));
    }
    for (index, listener) in stalled[ROUNDS..].iter().enumerate() {
        let address = listener.local_addr().unwrap();
        transport.hints.observe(
            &Hints::new([index as u8 + 82; 32]).wire(address.port()),
            address,
            now,
        );
    }
    // Still within the first two dials' stagger, the enrollee dials its other two.
    for tick in 0..INCOMING as u32 {
        transport
            .tick(now + Duration::from_millis(100) + SPACING * tick)
            .unwrap();
    }
    assert!(Duration::from_millis(100) + SPACING * (INCOMING as u32 - 1) < STAGGER);
    assert_eq!(
        transport.workers.len(),
        rounds,
        "a pending device has no incoming traffic to reserve workers for"
    );
    for listener in &stalled[ROUNDS..] {
        held.push(accept_ready(listener));
    }
    assert_eq!(held.len(), rounds);
    transport.stop();
}

/// CR2: two endpoints that accept the connection and never answer it (a peer whose accept queue
/// is full) take both places a serving node's rounds have, ahead of a real peer. Once the first
/// has staggered, the peer is dialled beside them, and its round completes. The ticks run on an
/// injected clock and the dial is checked as the tick returns; only the sockets run in real time.
///
/// NEGATIVE CONTROL (run here, [`super::UNSTAGGERED`]): while both stalled dials hold their places
/// the peer is not dialled.
#[test]
fn a_peer_is_dialled_and_repairs_beside_two_stalled_dials() {
    for unstaggered in [true, false] {
        let lab = Lab::new();
        let mut listeners: Vec<_> = (0..3)
            .map(|_| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
            .collect();
        listeners.sort_by_key(|listener| listener.local_addr().unwrap());
        let peer = listeners.pop().unwrap();
        let address = peer.local_addr().unwrap();
        let mut transport = loopback(lab.endpoints[0].clone(), 0, [61; 32]);
        let start = Instant::now();
        // One loopback address beacons at most two endpoints, so the first stalled dial is
        // already open, and the second stalled endpoint and the peer are heard.
        let mut held = Vec::new();
        transport.workers.push(
            Worker::start(
                lab.endpoints[0].clone(),
                None,
                Some(listeners[0].local_addr().unwrap()),
                Scope::Loopback,
                None,
                Repair::default(),
                start,
            )
            .unwrap(),
        );
        held.push(accept_ready(&listeners[0]));
        for (index, listener) in [&listeners[1], &peer].into_iter().enumerate() {
            let at = listener.local_addr().unwrap();
            transport.hints.observe(
                &Hints::new([index as u8 + 62; 32]).wire(at.port()),
                at,
                start,
            );
        }
        super::UNSTAGGERED.with(|cell| cell.set(unstaggered));
        transport.tick(start + Duration::from_millis(300)).unwrap();
        assert_eq!(
            transport.workers.len(),
            2,
            "the second stalled endpoint is dialled"
        );
        held.push(accept_ready(&listeners[1]));
        let staggered = start + STAGGER + Duration::from_millis(100);
        transport.tick(staggered).unwrap();
        super::UNSTAGGERED.with(|cell| cell.set(false));
        let dialled = transport
            .workers
            .iter()
            .any(|worker| worker.target == Some(address));
        if unstaggered {
            assert!(!dialled, "MUTANT: two stalled dials hold the peer's dial");
            transport.stop();
            continue;
        }
        assert!(dialled, "the peer waits behind two stalled dials");
        // Beside both: each holds its own dial until its handshake limit, which the peer's round
        // need not wait for.
        assert_eq!(transport.workers.len(), 3);
        let server = Worker::start(
            lab.endpoints[1].clone(),
            Some(accept_ready(&peer)),
            None,
            Scope::Loopback,
            None,
            Repair::default(),
            staggered,
        )
        .unwrap();
        assert!(
            server.task.join().unwrap().2,
            "the peer's side of the round"
        );
        let until = Instant::now() + Duration::from_secs(60);
        while !transport.hints.established(address) {
            assert!(Instant::now() < until, "the peer's round did not complete");
            transport.tick(staggered).unwrap();
            aterm_uds::deadline::poll_yield();
        }
        transport.stop();
        drop(held);
    }
}

#[test]
fn completed_round_metadata_is_bounded_and_discloses_every_eviction() {
    let mut transport = Transport::isolated(None, 0, None).unwrap();
    for _ in 0..100 {
        transport.retain_report(RepairRound::panicked(None));
    }
    let reports: Vec<_> = transport.reports().collect();
    assert_eq!(reports.len(), 64);
    assert_eq!(
        reports
            .iter()
            .map(|report| report.dropped_before)
            .sum::<usize>(),
        36
    );
    assert!(transport.reports().next().is_none());
    let error = super::super::report::bounded_error("λ".repeat(1000));
    assert!(error.len() <= 512);
    assert!(error.ends_with('…'));
}
fn loopback_mode(endpoint: Option<Arc<Endpoint>>, port: u16, token: [u8; 32]) -> Transport {
    let listener = if endpoint
        .as_ref()
        .is_some_and(|endpoint| !endpoint.pending())
    {
        Some(Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
    } else {
        None
    };
    let hints = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, port, Scope::Loopback).unwrap();
    let now = Instant::now();
    Transport {
        endpoint,
        links: vec![Link {
            source: Ipv4Addr::LOCALHOST.into(),
            listener,
            hints,
            delivering: None,
        }],
        hints: Hints::new(token),
        workers: vec![],
        beacon: now,
        dial: now,
        rotate: now + Duration::from_secs(300),
        versions_at: now,
        listened: false,
        held: aterm_net::fleet_member::HeldSignatures::default(),
        bus: Bus::default(),
        scope: Scope::Loopback,
        port,
        contacts: Vec::new(),
        completed: VecDeque::new(),
        push: None,
        failure: None,
        doorbell: None,
        announced: Arc::default(),
        stand_in: None,
        delivering: None,
        observed: std::collections::BTreeMap::new(),
        finished_rounds: 0,
        memory: None,
    }
}

#[test]
fn unconfigured_transports_see_opaque_presence_without_opening_repair() {
    let mut a = loopback_mode(None, 0, [41; 32]);
    let mut b = loopback_mode(None, a.links[0].hints.port(), [42; 32]);
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let now = Instant::now();
        a.tick(now).unwrap();
        b.tick(now).unwrap();
        if a.unverified_peers() == 1 && b.unverified_peers() == 1 {
            break;
        }
        assert!(Instant::now() < until, "opaque presence did not arrive");
        thread::sleep(Duration::from_millis(1));
    }
    for transport in [&mut a, &mut b] {
        assert!(transport.workers.is_empty());
        assert!(transport.take_contacts().is_empty());
        assert!(transport.links.iter().all(|link| link.listener.is_none()));
    }
}

#[test]
fn listen_only_discovery_never_dials_or_promotes_hints_and_expires_them() {
    let lab = Lab::new();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [1; 32]);
    transport.endpoint = None;
    transport.links[0].listener = None;
    let now = Instant::now();
    transport.hints.observe(
        &Hints::new([2; 32]).wire(4000),
        "127.0.0.2:5000".parse().unwrap(),
        now,
    );
    transport.tick(now).unwrap();
    assert_eq!(transport.unverified_peers(), 1);
    assert!(transport.workers.is_empty());
    assert!(transport.take_contacts().is_empty());
    assert!(transport.links.iter().all(|link| link.listener.is_none()));
    transport.tick(now + Duration::from_secs(46)).unwrap();
    assert_eq!(transport.unverified_peers(), 0);
}

#[test]
fn pending_discovery_enrolls_without_advertising_and_then_repairs_application_data() {
    use crate::{
        catalog::Directory,
        table::{Session, store::Store},
    };
    let lab = crate::control::enrollment::tests::Lab::new(true);
    let mut a = loopback(Arc::new(lab.endpoint(0)), 0, [71; 32]);
    let port = a.links[0].hints.port();
    let mut b = loopback(Arc::new(lab.pending()), port, [72; 32]);
    let pending = lab.bootstrap(1);
    assert!(b.links[0].listener.is_none());
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        a.tick(Instant::now()).unwrap();
        b.tick(Instant::now()).unwrap();
        if b.workers.is_empty() && pending.witness().is_ok_and(|w| w.is_some()) {
            break;
        }
        assert!(
            Instant::now() < until,
            "pending device never obtained a witness"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        a.peers(),
        0,
        "pending device emitted no discovery advertisement"
    );
    drop(b);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory
        .open_application(&session, "documents", "notes", 1)
        .unwrap();
    Store::open(&session, &directory.path(scope).unwrap(), scope)
        .unwrap()
        .commit_catalog(&session, b"first automatic enrollment")
        .unwrap();
    drop(session);
    let mut b = loopback(Arc::new(lab.endpoint(1)), port, [73; 32]);
    converge(
        &mut a,
        &mut b,
        scope.digest(),
        1,
        "the enrolled device's default application",
    );
}

/// Tick both owners until `b` reports `count` operations of `table` held. Reaching it takes
/// several application rounds in turn (discover the table, repair it, report it held), and
/// opposite dials refuse each other's lease, so the hang detector is a minute with no new
/// application report on either side, not a budget for all of them.
fn converge(
    a: &mut Transport,
    b: &mut Transport,
    table: astream_wire::sync::Digest,
    count: usize,
    what: &str,
) {
    let mut until = Instant::now() + Duration::from_secs(60);
    let mut reports = (a.observed.clone(), b.observed.clone());
    let mut now = Instant::now();
    loop {
        a.tick(now).unwrap();
        b.tick(now).unwrap();
        // Observe a completed inventory without taking custody locks from
        // the workers. Validate the actual store after both owners stop.
        if b.observed.get(&table).copied().unwrap_or(0) >= count {
            return;
        }
        if (&a.observed, &b.observed) != (&reports.0, &reports.1) {
            reports = (a.observed.clone(), b.observed.clone());
            until = Instant::now() + Duration::from_secs(60);
        }
        assert!(
            Instant::now() < until,
            "{what}: repair stalled at {count}: {:?}\n{:?}",
            a.hints,
            b.hints
        );
        // Exercise delivery rather than the retry delay. The scheduler's
        // injected clock advances when idle; wall time only detects hangs.
        if a.workers.is_empty() && b.workers.is_empty() {
            now += crate::runtime::TICK;
        } else {
            now = now.max(Instant::now());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_round_refused_before_versions_never_hides_the_round_it_lost_to() {
    let own = Said::Version(Version::own());
    let start = Instant::now();
    let after = |ms| start + Duration::from_millis(ms);
    // A crossed dial: the loser authenticated just after the round it lost to and finished
    // first, refused at admission before versions were traded.
    let mut contact = Contact::new([7; 48], after(2), Said::Unknown, None);
    contact.heard(after(1), own);
    assert_eq!(
        (contact.at, contact.said, contact.said_at),
        (after(2), own, after(1))
    );
    // A later round that traded nothing keeps that version, as does an older one that said
    // nothing valid; a newer one that said nothing valid replaces it.
    contact.heard(after(5), Said::Unknown);
    contact.heard(after(0), Said::Nothing);
    assert_eq!(
        (contact.at, contact.said, contact.said_at),
        (after(5), own, after(1))
    );
    contact.heard(after(3), Said::Nothing);
    assert_eq!(
        (contact.at, contact.said, contact.said_at),
        (after(5), Said::Nothing, after(3))
    );
    // Rounds that all said nothing of their version leave nothing to record apart from the
    // latest contact, whichever order they finished in: each fold carries said_at to the
    // contact it has just raised, not the one before it.
    for order in [[5, 1], [1, 5]] {
        let mut silent = Contact::new([7; 48], after(2), Said::Unknown, None);
        for ms in order {
            silent.heard(after(ms), Said::Unknown);
        }
        assert_eq!(
            (silent.at, silent.said, silent.said_at),
            (after(5), Said::Unknown, after(5)),
            "silent rounds folded at {order:?}"
        );
    }
}

/// Of the readings a peer's rounds took, a contact keeps the one its latest-authenticated round
/// took, whichever round finished first, and though a clock set back dates it earlier; a round
/// that took none keeps it.
///
/// Negative controls: with `Contact::read` keeping the reading of the last round to finish, the
/// earlier round's reading replaced the later one's (`left: Some(Reading { .., at_ms: 1000, ..
/// })`, `right: Some(Reading { .., at_ms: 2000, .. })`); with it keeping the later wall time
/// (`held.at_ms <= clock.at_ms`), the reading taken after the clock was set back was dropped
/// (`left: Some(Reading { .., at_ms: 3000, .. })`, `right: Some(Reading { .., at_ms: 500, ..
/// })`).
#[test]
fn a_contact_keeps_the_latest_clock_reading() {
    let reading = |at_ms| crate::clock::Reading {
        lo_ms: -1,
        hi_ms: 0,
        at_ms,
        source: crate::clock::Source::Mark,
    };
    // When each round authenticated.
    let start = Instant::now();
    let round = |n: u64| start + Duration::from_secs(n);
    let mut contact = Contact::new([7; 48], round(2), Said::Unknown, Some(reading(2000)));
    contact.read(round(1), Some(reading(1000)));
    contact.read(round(3), None);
    assert_eq!(contact.clock, Some(reading(2000)));
    contact.read(round(3), Some(reading(3000)));
    assert_eq!(contact.clock, Some(reading(3000)));
    // The clock is set back two seconds' worth: the next round's reading is dated earlier.
    contact.read(round(4), Some(reading(500)));
    assert_eq!(contact.clock, Some(reading(500)));
}

#[test]
fn authenticated_transport_contacts_become_unconfirmed_retained_network_history() {
    use crate::{
        catalog::Directory,
        network_state::{ObservationHandle, Observer, read_history},
        table::{Session, store::Store},
    };
    use aterm_secrets::commons::roster::Role;
    use aterm_types::network_state::{HistoryState, Reachability, Visibility};

    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let remote = lab.bootstrap(1);
    let expected = super::super::machine_identity(remote.request().unwrap().binding());
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = directory
        .open_application(&session, "accounts", "proposals", 1)
        .unwrap();
    Store::open(&session, &directory.path(scope).unwrap(), scope)
        .unwrap()
        .commit_catalog(&session, b"contact observation")
        .unwrap();
    drop(session);
    let endpoint = |n| {
        Arc::new(
            Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config"))
                .unwrap(),
        )
    };
    let mut a = loopback(endpoint(0), 0, [81; 32]);
    let port = a.links[0].hints.port();
    let mut b = loopback(endpoint(1), port, [82; 32]);
    converge(
        &mut a,
        &mut b,
        scope.digest(),
        1,
        "authenticated observer contact",
    );
    let until = Instant::now() + Duration::from_secs(60);
    while !a.workers.is_empty() || !b.workers.is_empty() {
        a.tick(Instant::now()).unwrap();
        b.tick(Instant::now()).unwrap();
        assert!(
            Instant::now() < until,
            "authenticated exchanges did not finish"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let contacts = a.take_contacts();
    assert!(
        contacts
            .iter()
            .any(|contact| contact.actor == remote.request().unwrap().binding().actor())
    );
    let newest = contacts
        .into_iter()
        .max_by_key(|contact| contact.at)
        .unwrap();
    assert_eq!(newest.actor, remote.request().unwrap().binding().actor());
    // The completed round traded signed versions (C26), and each side read the other's clock
    // mark (F22): one clock, so they agree. Negative control: with `finish` carrying no reading
    // into the contacts, `left: None`, `right: Some((Mark, Agree))`.
    assert_eq!(newest.said, Said::Version(Version::own()));
    assert_eq!(
        newest
            .clock
            .map(|clock| (clock.source, clock.class(crate::clock::Standing::Unknown))),
        Some((crate::clock::Source::Mark, crate::clock::Class::Agree))
    );
    let contact = (newest.actor, newest.at);
    // Drive freshness from the real exchange's timestamp, not test execution speed.
    let at = contact.1;
    let wall = 100_000;

    let config = lab.paths[0].home.join("network-contact-test.toml");
    let published = ObservationHandle::default();
    let mut observer = Observer::open(&config, published.clone()).unwrap();
    observer
        .local(
            super::super::machine_identity(bootstrap.request().unwrap().binding()),
            bootstrap.trust(),
        )
        .unwrap();
    super::super::observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut observer,
        vec![([0xff; 48], at)],
        &[],
        &super::super::place::Publisher::default(),
        || (wall, at),
    );
    observer.observe(
        Visibility::Active,
        0,
        "Authenticated test observation",
        wall,
        at,
    );
    assert!(published.snapshot().peers.is_empty());
    super::super::observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut observer,
        vec![contact],
        &[],
        &super::super::place::Publisher::default(),
        || (wall, at),
    );
    let mut round = RepairRound::panicked(Some("127.0.0.1:41384".parse().unwrap()));
    round.actor = Some(newest.actor);
    round.exchange_started = true;
    round.control = Ok(crate::control::Report::default());
    round.completed_at_ms = wall;
    let mut wrong = round.clone();
    wrong.actor = Some(bootstrap.request().unwrap().binding().actor());
    let mut unknown = round.clone();
    unknown.actor = Some([0xff; 48]);
    super::super::observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut observer,
        Vec::new(),
        &[wrong, unknown],
        &super::super::place::Publisher::default(),
        || (wall, at),
    );
    let mut failed = round.clone();
    failed.control = Err(Failure::new(
        aterm_types::network_state::RepairCause::Io,
        "failed after authentication",
    ));
    super::super::observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut observer,
        Vec::new(),
        &[failed],
        &super::super::place::Publisher::default(),
        || (wall, at),
    );
    observer.observe(Visibility::Active, 0, "before endpoint", wall, at);
    assert!(
        published.snapshot().peers[0]
            .last_authenticated_endpoint
            .is_none()
    );
    let endpoint = round.authenticated_endpoint().unwrap().1;
    super::super::observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut observer,
        Vec::new(),
        &[round],
        &super::super::place::Publisher::default(),
        || (wall, at),
    );
    observer.observe(
        Visibility::Active,
        0,
        "Authenticated test observation",
        wall,
        at,
    );
    let live = published.snapshot();
    assert_eq!(live.peers.len(), 1);
    assert_eq!(live.peers[0].identity, expected);
    assert_eq!(live.peers[0].reachability, Reachability::Reachable);
    assert_eq!(
        live.peers[0].last_authenticated_endpoint.as_ref(),
        Some(&endpoint)
    );
    let slot = live.peers[0].slot;
    let last_contact = live.peers[0].last_seen_ms;
    assert_eq!(last_contact, wall);

    drop(b);
    drop(a);
    observer.observe(
        Visibility::Offline,
        0,
        "Transport stopped",
        wall + 16_000,
        at + Duration::from_secs(16),
    );
    let offline = published.snapshot();
    assert_ne!(offline.peers[0].reachability, Reachability::Reachable);
    assert_eq!(offline.peers[0].last_seen_ms, last_contact);
    drop(observer);
    let retained = read_history(&config).unwrap();
    assert_eq!(retained.history, HistoryState::Ready);
    assert_eq!(retained.peers[0].identity, expected);
    assert_eq!(retained.peers[0].slot, slot);
    assert_eq!(retained.peers[0].last_seen_ms, last_contact);
    assert_eq!(retained.peers[0].reachability, Reachability::Unknown);
    assert_eq!(retained.peers[0].last_checked_ms, None);
    assert_eq!(
        retained.peers[0].last_authenticated_endpoint.as_ref(),
        Some(&endpoint)
    );
    let reopened = Observer::open(&config, published.clone()).unwrap();
    let restored = published.snapshot();
    assert_eq!(restored.observer, None);
    assert_eq!(restored.peers[0].identity, expected);
    assert_eq!(restored.peers[0].slot, slot);
    assert_eq!(restored.peers[0].last_seen_ms, last_contact);
    assert_eq!(restored.peers[0].reachability, Reachability::Unknown);
    drop(reopened);
}

#[test]
fn discovered_commons_tables_repair_without_a_peer_list_and_rejoin_after_offline_writes() {
    use crate::{
        catalog::Directory,
        table::{Session, store::Store},
    };
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = directory
        .open_application(&session, "accounts", "proposals", 1)
        .unwrap();
    Store::open(&session, &directory.path(scope).unwrap(), scope)
        .unwrap()
        .commit_catalog(&session, b"first proposal")
        .unwrap();
    drop(session);
    let endpoint = |n| {
        Arc::new(
            Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config"))
                .unwrap(),
        )
    };
    let mut a = loopback(endpoint(0), 0, [51; 32]);
    let port = a.links[0].hints.port();
    let mut b = loopback(endpoint(1), port, [52; 32]);
    converge(
        &mut a,
        &mut b,
        scope.digest(),
        1,
        "a discovered application",
    );
    drop(b);
    drop(a);
    let b_bootstrap = lab.bootstrap(1);
    let b_session = Session::open(&b_bootstrap).unwrap();
    let b_directory = Directory::new(&lab.paths[1], b_bootstrap.trust());
    assert_eq!(
        Store::open(&b_session, &b_directory.path(scope).unwrap(), scope)
            .unwrap()
            .inventory(&b_session)
            .unwrap()
            .count(),
        1
    );
    drop(b_session);
    let session = Session::open(&bootstrap).unwrap();
    Store::open(&session, &directory.path(scope).unwrap(), scope)
        .unwrap()
        .commit_catalog(&session, b"offline proposal")
        .unwrap();
    drop(session);
    let mut a = loopback(endpoint(0), 0, [53; 32]);
    let port = a.links[0].hints.port();
    let mut b = loopback(endpoint(1), port, [54; 32]);
    converge(
        &mut a,
        &mut b,
        scope.digest(),
        2,
        "a discovered application",
    );
    drop(b);
    drop(a);
    let b_session = Session::open(&b_bootstrap).unwrap();
    assert_eq!(
        Store::open(&b_session, &b_directory.path(scope).unwrap(), scope)
            .unwrap()
            .inventory(&b_session)
            .unwrap()
            .count(),
        2
    );
}

/// C3, through two serving transports and the production store, listener and rounds. Once the
/// two hold the same and nothing is due, a commit on this thread rings A's doorbell; the turn
/// that ring wakes counts one announce and starts a dial of B with the clock not advanced; and
/// B comes to hold the record. What B's round thread ingested for it announces nothing at B,
/// whose listener hears every append to its custody: B's bank stays empty to the end.
///
/// NEGATIVE CONTROL (2026-11-12): with the listener noting the table and ringing nothing, A's
/// wait stayed unready (`the commit rings the owner`).
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::announce`] leaving each endpoint's next dial
/// where it was, the woken turn started no dial (`the woken turn dials B at the same instant`:
/// `left: 0`, `right: 1`).
/// NEGATIVE CONTROL (2026-11-12): with the listener not reading whether its thread runs a
/// round, B announced the record its round brought it (`what a round brought B announces
/// nothing there`: `left: Some(({…}, false))`, `right: None`).
#[test]
fn a_local_append_dials_the_peer_at_once_and_what_the_round_brings_announces_nothing() {
    use crate::{
        catalog::Directory,
        table::{
            Session,
            store::{Store, claim_dir_of, listen},
        },
    };
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = directory
        .open_application(&session, "accounts", "proposals", 1)
        .unwrap();
    drop(session);
    let commit = |payload: &[u8]| {
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &directory.path(scope).unwrap(), scope)
            .unwrap()
            .commit_catalog(&session, payload)
            .unwrap();
    };
    commit(b"first proposal");
    let endpoint = |n| {
        Arc::new(
            Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config"))
                .unwrap(),
        )
    };
    let mut a = loopback(endpoint(0), 0, [61; 32]);
    a.doorbell = Some(Arc::new(Doorbell::new().unwrap()));
    let port = a.links[0].hints.port();
    let mut b = loopback(endpoint(1), port, [62; 32]);
    let _hearing = [
        listen(claim_dir_of(&bootstrap).unwrap(), a.announcer()).expect("a free slot"),
        listen(claim_dir_of(&lab.bootstrap(1)).unwrap(), b.announcer()).expect("a free slot"),
    ];
    let b_at = b.links[0].listener.as_ref().unwrap().local_addr().unwrap();
    // Tick both on one virtual clock until `done` holds with no round running: it advances a
    // tick while both rest and follows the wall clock while a round runs. Before each of B's
    // turns its bank is read, and must be empty. The hang detector is a minute with no round
    // finished on either side.
    let drive = |a: &mut Transport,
                 b: &mut Transport,
                 mut now: Instant,
                 what: &str,
                 done: &dyn Fn(&Transport, &Transport, Instant) -> bool| {
        let mut until = Instant::now() + Duration::from_secs(60);
        let mut finished = (a.finished_rounds, b.finished_rounds);
        loop {
            a.tick(now).unwrap();
            assert_eq!(
                b.announced.take(),
                None,
                "what a round brought B announces nothing there"
            );
            b.tick(now).unwrap();
            let idle = a.workers.is_empty() && b.workers.is_empty();
            if idle && done(a, b, now) {
                return now;
            }
            if (a.finished_rounds, b.finished_rounds) != finished {
                finished = (a.finished_rounds, b.finished_rounds);
                until = Instant::now() + Duration::from_secs(60);
            }
            assert!(Instant::now() < until, "{what}: {:?}", a.hints);
            if idle {
                now += crate::runtime::TICK;
            } else {
                now = now.max(Instant::now());
            }
            thread::sleep(Duration::from_millis(1));
        }
    };
    let held = |b: &Transport| b.observed.get(&scope.digest()).copied().unwrap_or(0);
    // B holds the first record, A has B's endpoint established, and neither A's next dial of
    // it nor its spacing is due.
    let now = drive(
        &mut a,
        &mut b,
        Instant::now(),
        "the first record",
        &|a, b, now| {
            held(b) >= 1
                && a.hints.established(b_at)
                && a.dial <= now
                && a.hints.next_at(b_at) > now
        },
    );
    a.clear_doorbell();
    a.turn(now, false).unwrap();
    assert_eq!(a.workers.len(), 0, "nothing is due before the commit");
    let ready = |a: &Transport| aterm_uds::ready::any_now(&a.waits_on().unwrap()).unwrap();
    assert!(ready(&a).is_empty(), "and nothing wakes the owner");
    let announces = census::cadence_here().announces;

    commit(b"second proposal");
    assert!(!ready(&a).is_empty(), "the commit rings the owner");
    a.clear_doorbell();
    a.turn(now, false).unwrap();
    assert_eq!(
        census::cadence_here().announces - announces,
        1,
        "the woken turn takes one announce"
    );
    assert_eq!(
        a.workers
            .iter()
            .filter(|worker| worker.target == Some(b_at))
            .count(),
        1,
        "the woken turn dials B at the same instant"
    );
    drive(&mut a, &mut b, now, "the second record", &|_, b, _| {
        held(b) >= 2
    });
    assert_eq!(b.announced.take(), None);
    assert_eq!(
        census::cadence_here().announces - announces,
        1,
        "one commit, one announce, on either side"
    );
}

#[test]
fn native_discovery_repairs_two_replicas_and_an_offline_replica_rejoins() {
    let lab = Lab::new();
    let mut a = loopback(lab.endpoints[0].clone(), 0, [1; 32]);
    let port = a.links[0].hints.port();
    let mut b = loopback(lab.endpoints[1].clone(), port, [2; 32]);
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        a.tick(Instant::now()).unwrap();
        b.tick(Instant::now()).unwrap();
        if a.workers.is_empty()
            && b.workers.is_empty()
            && a.peers() > 0
            && Journal::open_existing(&lab.dirs[1], lab.trust)
                .is_ok_and(|journal| journal.node(&lab.update.id()).is_some())
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "two replicas never repaired: hints={},{} workers={},{}",
            a.peers(),
            b.peers(),
            a.workers.len(),
            b.workers.len()
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        a.take_contacts()
            .iter()
            .any(|contact| contact.actor[32..] == [4; 16]),
        "completed authenticated exchanges, not beacon addresses, feed the observer"
    );
    drop(b);
    let after = Node::test_seal(
        &[21; 32],
        [9; 16],
        Edit::Update {
            parent: lab.update.id(),
            hosts: &[],
            payload: b"offline edit",
        },
        &[Identity::generate().unwrap().recipient()],
        0,
        &[],
    )
    .unwrap();
    // Local editing needs no transport owner or remote peer.
    loop {
        if let Ok(mut journal) = Journal::open_existing(&lab.dirs[0], lab.trust) {
            journal.ingest(after.bytes()).unwrap();
            break;
        }
        a.tick(Instant::now()).unwrap();
        assert!(Instant::now() < until, "local journal stayed locked");
        thread::sleep(Duration::from_millis(1));
    }
    let mut b = loopback(lab.endpoints[1].clone(), port, [3; 32]);
    loop {
        a.tick(Instant::now()).unwrap();
        b.tick(Instant::now()).unwrap();
        if a.workers.is_empty()
            && b.workers.is_empty()
            && a.peers() > 0
            && Journal::open_existing(&lab.dirs[1], lab.trust)
                .is_ok_and(|journal| journal.node(&after.id()).is_some())
        {
            break;
        }
        assert!(Instant::now() < until, "offline replica never repaired");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn failed_discovery_link_does_not_withdraw_the_healthy_listener() {
    let lab = Lab::new();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [1; 32]);
    let bad = loopback(lab.endpoints[0].clone(), 0, [2; 32])
        .links
        .remove(0);
    bad.hints.withdraw();
    transport.links.push(bad);
    transport.tick(Instant::now()).unwrap();
    assert_eq!(transport.interfaces(), 1);
    transport.links[0]
        .listener
        .as_ref()
        .unwrap()
        .recheck()
        .unwrap();
    transport.links[0].hints.recheck().unwrap();
}

#[test]
fn failed_transport_observation_replaces_the_active_reason_and_retains_history() {
    use super::super::{ObservationMode, observe_transport};
    use crate::network_state::{ObservationHandle, Observer};
    use aterm_types::network_state::{MachineIdentity, Reachability, Visibility};

    let root = aterm_tempfile::tempdir().unwrap();
    let handle = ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    let identity = MachineIdentity {
        machine_id: [23; 16],
        incarnation: [7; 16],
        label: None,
    };
    let contact_ms = aterm_time::unix_millis().unwrap();
    observer.contact(identity.clone(), contact_ms, Instant::now());
    let mut transport = loopback_mode(None, 0, [3; 32]);
    let modes = [
        ObservationMode::Repair,
        ObservationMode::Enrollment,
        ObservationMode::Discovery,
    ];
    let active: Vec<_> = modes
        .iter()
        .map(|mode| {
            observe_transport(&mut observer, &transport, *mode);
            let state = handle.snapshot();
            assert_eq!(state.visibility, Visibility::Active);
            state.detail
        })
        .collect();
    transport.links[0].hints.withdraw();
    let socket_error = transport.links[0].hints.recheck().unwrap_err().to_string();
    transport.tick(Instant::now()).unwrap();
    assert!(transport.unavailable());
    let unavailable: Vec<_> = modes
        .iter()
        .enumerate()
        .map(|(index, mode)| {
            observe_transport(&mut observer, &transport, *mode);
            let state = handle.snapshot();
            assert_eq!(state.visibility, Visibility::Unavailable);
            assert!(!state.detail.is_empty());
            assert!(state.detail.contains(&socket_error), "{}", state.detail);
            assert!(state.detail.contains("127.0.0.1"), "{}", state.detail);
            assert_ne!(state.detail, active[index]);
            assert_eq!(state.peers[0].identity, identity);
            assert_eq!(state.peers[0].last_seen_ms, contact_ms);
            assert_eq!(state.peers[0].reachability, Reachability::Unknown);
            state.detail
        })
        .collect();
    assert!(unavailable.windows(2).all(|pair| pair[0] != pair[1]));
    transport.refresh(&[]);
    assert!(!transport.unavailable());
    assert!(transport.failure().is_none());
    for (index, mode) in modes.iter().enumerate() {
        observe_transport(&mut observer, &transport, *mode);
        let state = handle.snapshot();
        assert_eq!(state.visibility, Visibility::Offline);
        assert_ne!(state.detail, active[index]);
        assert_ne!(state.detail, unavailable[index]);
        assert_eq!(state.peers[0].identity, identity);
        assert_eq!(state.peers[0].last_seen_ms, contact_ms);
    }
}

/// A network that fails a link's sends (EHOSTUNREACH) while its sockets bind: the link's next
/// beacon fails and the link is dropped; each poll's refresh binds it again, and it fails its
/// first beacon there, so between a refresh and the next tick it never reads as a usable
/// interface. The transport stays Unavailable through every poll, and a retained peer's presence
/// history takes no sample from it. Once the sends are delivered again, the next refresh opens
/// the link.
#[test]
fn an_undelivered_link_never_reads_active_between_its_refresh_and_its_send() {
    use super::super::{ObservationMode, TICK, observe_transport};
    use crate::network_state::{ObservationHandle, Observer};
    use aterm_types::network_state::{MachineIdentity, Visibility};
    use std::sync::atomic::{AtomicBool, Ordering};

    let delivering = Arc::new(AtomicBool::new(true));
    let mut transport = Transport::isolated(None, 0, Some(delivering.clone())).unwrap();
    assert_eq!(
        transport.interfaces(),
        1,
        "delivered: its first beacon went out"
    );
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    let identity = MachineIdentity {
        machine_id: [29; 16],
        incarnation: [5; 16],
        label: None,
    };
    observer.contact(identity, aterm_time::unix_millis().unwrap(), Instant::now());
    delivering.store(false, Ordering::Release);
    transport.tick(Instant::now() + BEACON).unwrap();
    assert!(transport.unavailable(), "its next beacon undelivered");
    observe_transport(&mut observer, &transport, ObservationMode::Discovery);
    let samples = handle.snapshot().peers[0].samples.clone();
    let loopback = [SocketAddr::from((Ipv4Addr::LOCALHOST, 0))];
    for poll in 0..3 {
        // As the owner's loop runs a poll: `now` first, then the refresh, then the ticks.
        let now = Instant::now();
        transport.refresh(&loopback);
        for tick in 0..2 {
            transport.tick(now + TICK * tick).unwrap();
            observe_transport(&mut observer, &transport, ObservationMode::Discovery);
            let state = handle.snapshot();
            assert_eq!(
                state.visibility,
                Visibility::Unavailable,
                "poll {poll}, tick {tick}: {}",
                state.detail
            );
            assert!(
                state
                    .detail
                    .starts_with("Local network sockets unavailable"),
                "{}",
                state.detail
            );
            assert_eq!(
                state.peers[0].samples, samples,
                "poll {poll}, tick {tick}: no presence sample"
            );
        }
    }
    delivering.store(true, Ordering::Release);
    let now = Instant::now();
    transport.refresh(&loopback);
    transport.tick(now).unwrap();
    assert_eq!(
        transport.interfaces(),
        1,
        "delivered: the next refresh opens it"
    );
    observe_transport(&mut observer, &transport, ObservationMode::Discovery);
    assert_eq!(handle.snapshot().visibility, Visibility::Active);
}

/// A peer whose port refuses the connection is dialed later each time, through the real tick,
/// worker and dial: 5, 10 and 20 s apart, each plus under 2 s of jitter, where contention keeps
/// 3-5 s. Virtual time drives the tick; only the dials themselves run. NEGATIVE CONTROL: with
/// `Outcome::of` calling an unauthenticated dial `Busy`, the dials come 3-5 s apart.
#[test]
fn a_peer_that_refuses_its_connections_is_dialed_later_each_time() {
    let lab = Lab::new();
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [41; 32]);
    let start = Instant::now();
    transport
        .hints
        .observe(&Hints::new([42; 32]).wire(dead.port()), dead, start);
    let mut dials = Vec::new();
    let mut at = start;
    while dials.len() < 4 && at < start + Duration::from_secs(44) {
        transport.tick(at).unwrap();
        if !transport.workers.is_empty() {
            dials.push(at - start);
            let until = Instant::now() + Duration::from_secs(60);
            while !transport
                .workers
                .iter()
                .all(|worker| worker.task.is_finished())
            {
                assert!(Instant::now() < until, "a refused dial did not finish");
                aterm_uds::deadline::poll_yield();
            }
            // Reaped at the same virtual instant: the next dial is due only after its wait.
            transport.tick(at).unwrap();
            assert!(transport.workers.is_empty(), "{dials:?}");
        }
        at += Duration::from_millis(100);
    }
    let waits: Vec<_> = dials.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let fives: Vec<_> = waits.iter().map(|wait| wait.as_secs() / 5).collect();
    assert_eq!(fives, [1, 2, 4], "dials at {dials:?}");
    assert!(
        transport.reports().all(|round| round.actor.is_none()),
        "a refused port authenticated a peer"
    );
}

/// What a dial says of its endpoint, over real sockets (CR2): a refused connection, or a listener
/// that proves another lab's discriminator, says no peer of this lab is there
/// ([`Repair::disowned`]); a listener that accepts and hangs up says nothing of the kind. Only a
/// peer that authenticates raises the dial's flag ([`Worker::heard`]).
#[test]
fn a_dial_disowns_its_endpoint_only_on_evidence() {
    let lab = Lab::new();
    let other = Lab::new();
    assert_ne!(
        lab.endpoints[0].beacon_key(),
        other.endpoints[1].beacon_key(),
        "two labs"
    );
    let dial = |target: SocketAddr| {
        Worker::start(
            lab.endpoints[0].clone(),
            None,
            Some(target),
            Scope::Loopback,
            None,
            Repair::default(),
            Instant::now(),
        )
        .unwrap()
    };
    let ended = |worker: Worker| {
        let heard = worker.heard.clone();
        let (_, repair, _, _) = worker.task.join().unwrap();
        (
            repair.disowned(),
            heard.load(std::sync::atomic::Ordering::Acquire),
        )
    };
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    assert_eq!(ended(dial(dead)), (true, false), "a refused port");

    let hangs_up = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let worker = dial(hangs_up.local_addr().unwrap());
    drop(
        aterm_uds::accept(
            &hangs_up,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap(),
    );
    assert_eq!(
        ended(worker),
        (false, false),
        "a connection closed unproved"
    );

    for (server, disowned, heard) in [
        (&other.endpoints[1], true, false),
        (&lab.endpoints[1], false, true),
    ] {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let (dialled, served) = round_at(&lab.endpoints[0], server, &listener, Repair::default());
        assert_eq!(ended(dialled), (disowned, heard), "disowned, heard");
        drop(served.task.join().unwrap());
    }
}

#[test]
fn panicked_worker_releases_the_hint_slot_for_retry() {
    let lab = Lab::new();
    let mut transport = loopback(lab.endpoints[0].clone(), 0, [1; 32]);
    let now = Instant::now();
    transport.hints.observe(
        &Hints::new([9; 32]).wire(1234),
        "127.0.0.1:1".parse().unwrap(),
        now,
    );
    let (address, _) = transport.hints.next(now).unwrap();
    transport.workers.push(Worker {
        channel: Arc::new(Mutex::new(Channel::default())),
        incoming: false,
        target: Some(address),
        task: thread::spawn(|| panic!("intentional worker failure")),
        started: now,
        heard: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ended: Arc::new(AtomicBool::new(false)),
    });
    let until = now + Duration::from_secs(60);
    while !transport.workers.is_empty() {
        transport.tick(Instant::now()).unwrap();
        assert!(Instant::now() < until, "failed worker did not finish");
        aterm_uds::deadline::poll_yield();
    }
    assert!(
        transport
            .hints
            .next(now + Duration::from_secs(10))
            .is_some()
    );
}

/// A dial's failure names what held it: the LAN broker's absence or refusal is `lan-held` (the
/// typed refusal survives `into_io`, while a bare `NotConnected` is only I/O), a dial that reached
/// no peer is `unreachable`, and any other I/O failure stays `io`.
#[test]
fn a_dial_failure_names_a_held_lan_apart_from_an_unreachable_peer() {
    use aterm_types::network_state::RepairCause;
    use aterm_uds::lan::{LanError, Unavailable};
    let cause = |error: io::Error| dial_failure(error).repair_cause();
    for why in [
        Unavailable::NotInstalled,
        Unavailable::Outdated { ours: 1, theirs: 2 },
        Unavailable::Refused("fixture".into()),
        Unavailable::Unchecked("fixture".into()),
    ] {
        assert_eq!(
            cause(LanError::Unavailable(why).into_io()),
            RepairCause::LanHeld
        );
    }
    for kind in [
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::TimedOut,
        io::ErrorKind::HostUnreachable,
        io::ErrorKind::NetworkUnreachable,
        io::ErrorKind::NetworkDown,
        io::ErrorKind::AddrNotAvailable,
    ] {
        assert_eq!(
            cause(io::Error::from(kind)),
            RepairCause::Unreachable,
            "{kind:?}"
        );
    }
    for kind in [io::ErrorKind::NotConnected, io::ErrorKind::InvalidInput] {
        assert_eq!(cause(io::Error::from(kind)), RepairCause::Io, "{kind:?}");
    }
}

/// A round's end rings the doorbell its transport's owner waits on ([`Transport::waits_on`]), and
/// raises the worker's `ended` first, so the tick that ring wakes takes the round: a dial to a
/// port nothing listens on ends at once, and the doorbell then polls ready, which a doorbell no
/// round rang does not. A ring that never comes reaches the hang detector's minute.
///
/// NEGATIVE CONTROL (2026-11-09): with [`Ended`]'s drop raising `ended` but not ringing, the wait
/// reached the minute (`a round's end rang the doorbell`).
#[test]
fn a_rounds_end_rings_its_transports_doorbell() {
    let lab = Lab::new();
    let bell = Arc::new(Doorbell::new().unwrap());
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target = closed.local_addr().unwrap();
    drop(closed);
    let worker = Worker::start(
        lab.endpoints[0].clone(),
        None,
        Some(target),
        Scope::Loopback,
        Some(bell.clone()),
        Repair::default(),
        Instant::now(),
    )
    .unwrap();
    let rang = aterm_uds::ready::any(&[bell.fd()], Some(Instant::now() + Duration::from_secs(60)))
        .unwrap();
    assert_eq!(rang, [0], "a round's end rang the doorbell");
    assert!(
        worker.ended.load(Ordering::Acquire),
        "the round's end is raised before its ring"
    );
    assert!(bell.clear());
    let (_, _, success, _) = worker.task.join().unwrap();
    assert!(!success, "nothing listens there");
}

/// An idle transport keeps no deadline sooner than its next beacon, and waits on its doorbell
/// and its listener: once a peer it may dial is heard, its next dial is due at once, and once
/// that peer is in its round, nothing is due before the beacon.
///
/// NEGATIVE CONTROL (2026-11-09): with [`Transport::next_due`] leaving its dial out, the heard
/// peer was not due (`a heard peer is dialled at once`).
#[test]
fn an_idle_transport_waits_for_its_beacon_and_a_heard_peer_for_its_dial() {
    let lab = Lab::new();
    let mut transport = Transport::unopened(None, Scope::Loopback, 0).unwrap();
    transport.endpoint = Some(lab.endpoints[0].clone());
    transport.refresh(&[SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]);
    let now = Instant::now();
    assert_eq!(transport.interfaces(), 1, "the loopback link opened");
    assert!(transport.next_due(now) <= transport.beacon);
    assert!(transport.next_due(now) > now, "nothing is due at once");
    assert_eq!(
        transport.waits_on().map(|fds| fds.len()),
        Some(2),
        "its doorbell and its listener"
    );
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target = closed.local_addr().unwrap();
    drop(closed);
    transport
        .hints
        .observe(&Hints::new([9; 32]).wire(target.port()), target, now);
    assert!(
        transport.next_due(now) <= now,
        "a heard peer is dialled at once"
    );
    transport.tick(now).unwrap();
    assert_eq!(transport.workers.len(), 1, "the heard peer is dialled");
    assert_eq!(
        transport.next_due(now),
        transport.beacon.min(transport.rotate),
        "with its one peer in a round, nothing is due before the beacon"
    );
    transport.stop();
}
