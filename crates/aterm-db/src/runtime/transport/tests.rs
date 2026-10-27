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
        contacts: Vec::new(),
        failed: false,
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
        let (_, repair, success) = client.task.join().unwrap();
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
        let (_, repair, success) = client.task.join().unwrap();
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
        }],
        hints: Hints::new(token),
        workers: vec![],
        beacon: now,
        dial: now,
        rotate: now + Duration::from_secs(300),
        scope: Scope::Loopback,
        contacts: Vec::new(),
        failed: false,
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
            .any(|(actor, _)| actor[32..] == [4; 16]),
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
