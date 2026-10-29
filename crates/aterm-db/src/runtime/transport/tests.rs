// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{
    age::Identity,
    commons::{
        binding::Request,
        constraints::Fact,
        journal::{Journal, Trust},
        node::{Edit, Node},
    },
};

struct Lab {
    _root: aterm_tempfile::TempDir,
    dirs: [std::path::PathBuf; 2],
    trust: Trust,
    endpoints: [Arc<Endpoint>; 2],
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
                thread::yield_now();
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
    assert!(client.task.join().unwrap().2);
    assert!(server.task.join().unwrap().2);
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
        scope: Scope::Loopback,
        port: 0,
        contacts: Vec::new(),
        completed: VecDeque::new(),
        failed: false,
        bound: false,
        stand_in: None,
        permission: None,
        observed: std::collections::BTreeMap::new(),
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
    assert!(!channel.install(&connection).unwrap());
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
    let now = Instant::now();
    let mut old_addresses = Vec::new();
    for listener in &listeners[..2] {
        let address = listener.local_addr().unwrap();
        hints.observe(&Hints::new([62; 32]).wire(address.port()), address, now);
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
        let (_, repair, success, _) = client.task.join().unwrap();
        assert!(server.task.join().unwrap().2);
        assert!(success);
        hints.finish(address, repair, success, now);
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
        let (_, repair, success, _) = client.task.join().unwrap();
        assert!(!success);
        hints.finish(address, repair, success, retry);
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
        scope: Scope::Loopback,
        port,
        contacts: Vec::new(),
        completed: VecDeque::new(),
        failed: false,
        bound: false,
        stand_in: None,
        permission: None,
        observed: std::collections::BTreeMap::new(),
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
            .any(|(actor, _, _)| *actor == remote.request().unwrap().binding().actor())
    );
    let (actor, at, said) = contacts.into_iter().max_by_key(|(_, at, _)| *at).unwrap();
    assert_eq!(actor, remote.request().unwrap().binding().actor());
    // The completed round traded signed versions (C26).
    assert_eq!(
        said,
        aterm_secrets::commons::hearing::Said::Version(
            aterm_secrets::commons::hearing::Version::own()
        )
    );
    let contact = (actor, at);
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
    super::super::observe_contacts(&bootstrap, &mut observer, vec![([0xff; 48], at)], || {
        (wall, at)
    });
    observer.observe(
        Visibility::Active,
        0,
        "Authenticated test observation",
        wall,
        at,
    );
    assert!(published.snapshot().peers.is_empty());
    super::super::observe_contacts(&bootstrap, &mut observer, vec![contact], || (wall, at));
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
            .any(|(actor, _, _)| actor[32..] == [4; 16]),
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
    });
    let until = now + Duration::from_secs(60);
    while !transport.workers.is_empty() {
        transport.tick(Instant::now()).unwrap();
        assert!(Instant::now() < until, "failed worker did not finish");
        thread::yield_now();
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
    let transport = Transport::isolated(None, held.local_addr().unwrap().port(), None).unwrap();
    assert_eq!(transport.interfaces(), 0, "no link opened");
    assert!(transport.bound(), "its socket was made: a LAN use");
    drop(held);
}
