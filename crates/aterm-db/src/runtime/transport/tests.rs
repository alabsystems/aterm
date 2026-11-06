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
        Repair::default(),
    )
    .unwrap();
    let connection = accept_ready(&listener);
    let server = Worker::start(
        lab.endpoints[0].clone(),
        Some(connection),
        None,
        Scope::Loopback,
        Repair::default(),
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
        Repair::default(),
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
        scope: Scope::Loopback,
        port: 0,
        contacts: Vec::new(),
        completed: VecDeque::new(),
        push: None,
        failure: None,
        bound: false,
        stand_in: None,
        permission: None,
        observed: std::collections::BTreeMap::new(),
        finished_rounds: 0,
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
            repair,
        )
        .unwrap();
        let server = Worker::start(
            lab.endpoints[0].clone(),
            Some(accept_ready(&listener)),
            None,
            Scope::Loopback,
            Repair::default(),
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
                repair,
            )
            .unwrap();
            let server = Worker::start(
                lab.endpoints[0].clone(),
                Some(accept_ready(listener)),
                None,
                Scope::Loopback,
                Repair::default(),
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
            repair,
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
        repair,
    )
    .unwrap();
    let server = Worker::start(
        lab.endpoints[0].clone(),
        Some(accept_ready(&current)),
        None,
        Scope::Loopback,
        Repair::default(),
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
        permission: None,
    });
    let mut held = vec![std::net::TcpStream::connect(first).unwrap()];
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
    // A loopback `connect` can return before the kernel queues the connection at its listener;
    // tick only once each attachment really holds one waiting.
    for link in &transport.links {
        let listener = link.listener.as_ref().unwrap();
        assert!(
            listener.pending_within(Duration::from_secs(60)).unwrap(),
            "a connection to {} reaches its queue",
            listener.local_addr().unwrap()
        );
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
                Repair::default(),
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
            repair,
        )
        .unwrap();
        let server = Worker::start(
            lab.endpoints[1].clone(),
            Some(accept_ready(&known)),
            None,
            Scope::Loopback,
            Repair::default(),
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
            Repair::default(),
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
        Repair::default(),
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
    let stalled: Vec<_> = (0..WORKERS)
        .map(|_| Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap())
        .collect();
    let mut transport = loopback(endpoint.clone(), 0, [81; 32]);
    assert!(transport.links.iter().all(|link| link.listener.is_none()));
    let mut held = Vec::new();
    for listener in &stalled[..WORKERS / 2] {
        transport.workers.push(
            Worker::start(
                endpoint.clone(),
                None,
                Some(listener.local_addr().unwrap()),
                Scope::Loopback,
                Repair::default(),
            )
            .unwrap(),
        );
        held.push(accept_ready(listener));
    }
    let now = Instant::now() + Duration::from_secs(1);
    for (index, listener) in stalled[WORKERS / 2..].iter().enumerate() {
        let address = listener.local_addr().unwrap();
        transport.hints.observe(
            &Hints::new([index as u8 + 82; 32]).wire(address.port()),
            address,
            now,
        );
    }
    for tick in 0..WORKERS / 2 {
        transport
            .tick(now + Duration::from_secs(tick as u64))
            .unwrap();
    }
    assert_eq!(
        transport.workers.len(),
        WORKERS,
        "a pending device has no incoming traffic to reserve workers for"
    );
    for listener in &stalled[WORKERS / 2..] {
        held.push(accept_ready(listener));
    }
    assert_eq!(held.len(), WORKERS);
    transport.stop();
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
    let error = bounded_error("λ".repeat(1000));
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
            permission: None,
        }],
        hints: Hints::new(token),
        workers: vec![],
        beacon: now,
        dial: now,
        rotate: now + Duration::from_secs(300),
        versions_at: now,
        scope: Scope::Loopback,
        port,
        contacts: Vec::new(),
        completed: VecDeque::new(),
        push: None,
        failure: None,
        bound: false,
        stand_in: None,
        permission: None,
        observed: std::collections::BTreeMap::new(),
        finished_rounds: 0,
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
    // latest contact, whichever order they finished in.
    let mut silent = Contact::new([7; 48], after(2), Said::Unknown, None);
    silent.heard(after(5), Said::Unknown);
    silent.heard(after(1), Said::Unknown);
    assert_eq!(
        (silent.at, silent.said, silent.said_at),
        (after(5), Said::Unknown, after(5))
    );
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
    failed.control = Err("failed after authentication".into());
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
            observe_transport(&mut observer, &transport, *mode, false);
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
            observe_transport(&mut observer, &transport, *mode, false);
            let state = handle.snapshot();
            assert_eq!(state.visibility, Visibility::Unavailable);
            assert!(!state.detail.is_empty());
            assert!(state.detail.contains(&socket_error), "{}", state.detail);
            assert!(state.detail.contains("127.0.0.1"), "{}", state.detail);
            assert_ne!(state.detail, active[index]);
            assert_eq!(state.peers[0].identity, identity);
            assert_eq!(state.peers[0].last_seen_ms, contact_ms);
            assert_eq!(state.peers[0].reachability, Reachability::Unknown);
            observe_transport(&mut observer, &transport, *mode, true);
            let refused = handle.snapshot();
            assert_eq!(
                refused.visibility,
                Visibility::Unavailable,
                "a heard refusal"
            );
            assert!(
                refused.detail.starts_with(super::super::LAN_REFUSED),
                "failed links with a refusal heard: {}",
                refused.detail
            );
            assert_eq!(refused.peers[0].identity, identity);
            state.detail
        })
        .collect();
    assert!(unavailable.windows(2).all(|pair| pair[0] != pair[1]));
    transport.refresh(&[]);
    assert!(!transport.unavailable());
    assert!(transport.failure().is_none());
    for (index, mode) in modes.iter().enumerate() {
        observe_transport(&mut observer, &transport, *mode, false);
        let state = handle.snapshot();
        assert_eq!(state.visibility, Visibility::Offline);
        assert_ne!(state.detail, active[index]);
        assert_ne!(state.detail, unavailable[index]);
        assert_eq!(state.peers[0].identity, identity);
        assert_eq!(state.peers[0].last_seen_ms, contact_ms);
        observe_transport(&mut observer, &transport, *mode, true);
        let heard = handle.snapshot();
        assert_eq!(heard.visibility, Visibility::Offline, "a heard refusal");
        assert_eq!(
            heard.detail, state.detail,
            "with no address, no usable interface is the cause, whatever the records heard"
        );
    }
}

/// The OS refuses an app the LAN by failing its sends (EHOSTUNREACH) while its sockets bind. Once
/// the person turns aterm off, the link's next beacon fails and the link is dropped; each poll's
/// refresh binds it again, and it fails its first beacon there, so between a refresh and the next
/// tick it never reads as a usable interface. A refused transport stays Unavailable through every
/// poll, saying the refusal its records hear, and a retained peer's presence history takes no
/// sample from it. Once the OS allows the sends again, the next refresh opens the link.
#[test]
fn a_refused_link_never_reads_active_between_its_refresh_and_its_send() {
    use super::super::{LAN_REFUSED, ObservationMode, TICK, observe_transport};
    use crate::network_state::{ObservationHandle, Observer};
    use aterm_types::network_state::{MachineIdentity, Visibility};
    use std::sync::atomic::{AtomicBool, Ordering};

    let permission = Arc::new(AtomicBool::new(true));
    let mut transport = Transport::isolated(None, 0, Some(permission.clone())).unwrap();
    assert_eq!(
        transport.interfaces(),
        1,
        "allowed: its first beacon went out"
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
    permission.store(false, Ordering::Release);
    transport.tick(Instant::now() + BEACON).unwrap();
    assert!(transport.unavailable(), "its next beacon refused");
    observe_transport(&mut observer, &transport, ObservationMode::Discovery, true);
    let samples = handle.snapshot().peers[0].samples.clone();
    let loopback = [SocketAddr::from((Ipv4Addr::LOCALHOST, 0))];
    for poll in 0..3 {
        // As the owner's loop runs a poll: `now` first, then the refresh, then the ticks.
        let now = Instant::now();
        transport.refresh(&loopback);
        for tick in 0..2 {
            transport.tick(now + TICK * tick).unwrap();
            observe_transport(&mut observer, &transport, ObservationMode::Discovery, true);
            let state = handle.snapshot();
            assert_eq!(
                state.visibility,
                Visibility::Unavailable,
                "poll {poll}, tick {tick}: {}",
                state.detail
            );
            assert!(state.detail.starts_with(LAN_REFUSED), "{}", state.detail);
            assert_eq!(
                state.peers[0].samples, samples,
                "poll {poll}, tick {tick}: no presence sample"
            );
        }
    }
    permission.store(true, Ordering::Release);
    let now = Instant::now();
    transport.refresh(&loopback);
    transport.tick(now).unwrap();
    assert_eq!(
        transport.interfaces(),
        1,
        "allowed: the next refresh opens it"
    );
    observe_transport(&mut observer, &transport, ObservationMode::Discovery, true);
    assert_eq!(handle.snapshot().visibility, Visibility::Active);
}

/// Peers a repairing transport heard before the OS refused it the LAN mid-session (the person
/// turned aterm off in System Settings) do not hide that refusal: with no link left nothing is
/// heard now, and with no link to dial them the transport keeps their hints for good. From its
/// first failed beacon on, through every later poll, its observation says the refusal its records
/// hear.
#[test]
fn a_refusal_made_mid_session_is_said_whatever_peers_were_heard_before_it() {
    use super::super::{LAN_REFUSED, ObservationMode, POLL, observe_transport};
    use crate::network_state::{ObservationHandle, Observer};
    use aterm_types::network_state::Visibility;
    use std::sync::atomic::{AtomicBool, Ordering};

    let lab = Lab::new();
    let permission = Arc::new(AtomicBool::new(true));
    let mut transport = Transport::unopened(None, Scope::Loopback, 0).unwrap();
    transport.endpoint = Some(lab.endpoints[0].clone());
    transport.permission = Some(permission.clone());
    let loopback = [SocketAddr::from((Ipv4Addr::LOCALHOST, 0))];
    transport.refresh(&loopback);
    assert_eq!(transport.interfaces(), 1, "allowed: its link opened");
    let now = Instant::now();
    transport.hints.observe(
        &Hints::new([9; 32]).wire(1234),
        "127.0.0.1:1".parse().unwrap(),
        now,
    );
    assert_eq!(transport.peers(), 1, "a peer heard while allowed");
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    permission.store(false, Ordering::Release);
    transport.tick(now + BEACON).unwrap();
    assert!(transport.unavailable(), "its next beacon refused");
    assert_eq!(transport.peers(), 1, "the hint outlives the link");
    for poll in 0..4u32 {
        // Past every hint's expiry by the last poll.
        let at = now + BEACON + POLL * poll + Duration::from_secs(300) * poll;
        if poll > 0 {
            transport.refresh(&loopback);
            transport.tick(at).unwrap();
        }
        observe_transport(&mut observer, &transport, ObservationMode::Repair, true);
        let state = handle.snapshot();
        assert_eq!(state.visibility, Visibility::Unavailable, "poll {poll}");
        assert_eq!(state.detail, LAN_REFUSED, "poll {poll}");
    }
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

/// A bind that failed once its socket was made is a LAN use ([`Transport::bound`]): its bind or
/// join ran, and the OS may have asked its Local Network question there, as when an attachment
/// changes between the join and its re-check. Its link opens nothing, yet the transport reached
/// the LAN, so a host that held a dialog slot for that use keeps holding it for the question.
#[test]
fn a_bind_that_failed_after_its_socket_reached_the_lan() {
    // A socket with no address reuse holds the group's port: the join's socket binds and fails.
    let held = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    let mut transport = Transport::isolated(None, held.local_addr().unwrap().port(), None).unwrap();
    assert_eq!(transport.interfaces(), 0, "no link opened");
    assert!(transport.bound(), "its socket was made: a LAN use");
    assert!(transport.unavailable());
    assert!(
        transport.failure().unwrap().contains("multicast bind"),
        "{:?}",
        transport.failure()
    );
    drop(held);
    transport.refresh(&["127.0.0.1:0".parse().unwrap()]);
    assert_eq!(transport.interfaces(), 1, "the released port opens");
    assert!(!transport.unavailable());
    assert!(transport.failure().is_none(), "no stale bind error");
}
