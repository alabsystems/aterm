// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_previous_round_actor_is_not_a_new_authenticated_contact() {
    let mut repair = Repair::default();
    let trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };
    let actor = [4; 48];
    repair.prepare(trust, actor);
    assert_eq!(repair.authenticated_actor(), Some(actor));
    repair.application = Some(crate::replica::Report::default());
    repair.application_error = Some("previous upgrade failed".into());
    repair.membership = Some(Report::default());
    repair.membership_error = Some("previous membership exchange failed".into());
    repair.phase = Phase::Complete;
    repair.begin_round();
    assert_eq!(repair.phase(), Phase::Connecting);
    assert_eq!(
        repair.actor(),
        Some(actor),
        "retain repair scheduling hints"
    );
    assert_eq!(
        repair.authenticated_actor(),
        None,
        "a failed new connection cannot refresh presence"
    );
    assert!(repair.application().is_none());
    assert!(repair.application_error().is_none());
    assert!(repair.membership().is_none());
    assert!(repair.membership_error().is_none());
}
use aterm_net::lan::{Connection, Scope};
use aterm_secrets::{
    age::Identity,
    commons::{journal::State, node::Edit},
};
use std::{io::Cursor, net::TcpListener, thread};

#[derive(Clone, Copy)]
pub(super) enum PeerCacheStage {
    Acquired(Side),
    Waiting,
}

type PeerCacheHook = Box<dyn FnMut(PeerCacheStage)>;
thread_local! {
    static PEER_CACHE_HOOK: std::cell::RefCell<Option<PeerCacheHook>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn peer_cache_stage(stage: PeerCacheStage) {
    PEER_CACHE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(stage);
        }
    });
}

fn observe_peer_cache<T>(hook: impl FnMut(PeerCacheStage) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<PeerCacheHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PEER_CACHE_HOOK.with(|hook| *hook.borrow_mut() = self.0.take());
        }
    }
    let previous = PEER_CACHE_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
    let _restore = Restore(previous);
    run()
}

#[test]
fn crossed_native_tls_dials_finish_one_channel_after_both_responders_hold_leases() {
    use std::sync::mpsc;

    let lab = Lab::new();
    let update = lab.update(lab.genesis.id(), b"crossed-native-dial-control-repair", &[]);
    {
        let mut source = lab.journal(0);
        source.ingest(lab.genesis.bytes()).unwrap();
        source.ingest(update.bytes()).unwrap();
    }
    let endpoints = [lab.endpoint(0), lab.endpoint(1)];
    let low = usize::from(endpoints[0].actor > endpoints[1].actor);
    let high = 1 - low;
    let low_endpoint = &endpoints[low];
    let high_endpoint = &endpoints[high];
    let low_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let high_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let low_address = low_listener.local_addr().unwrap();
    let high_address = high_listener.local_addr().unwrap();
    let (low_ready, low_observed) = mpsc::channel();
    let (high_ready, high_observed) = mpsc::channel();
    let (waiting, observed) = mpsc::channel();
    thread::scope(|scope| {
        let losing_responder = scope.spawn(move || {
            observe_peer_cache(
                move |stage| {
                    if matches!(stage, PeerCacheStage::Acquired(Side::Responder)) {
                        low_ready.send(()).unwrap();
                        high_observed.recv_timeout(Duration::from_secs(60)).unwrap();
                        // Keep this real nonpreferred responder lease until the
                        // opposite preferred initiator has actually contended.
                        observed.recv_timeout(Duration::from_secs(60)).unwrap();
                    }
                },
                || {
                    low_endpoint.exchange(
                        Connection::admit(low_listener.accept().unwrap().0, Scope::Loopback)
                            .unwrap(),
                        Side::Responder,
                        &mut Repair::default(),
                    )
                },
            )
        });
        let winning_responder = scope.spawn(move || {
            observe_peer_cache(
                move |stage| {
                    if matches!(stage, PeerCacheStage::Acquired(Side::Responder)) {
                        high_ready.send(()).unwrap();
                        low_observed.recv_timeout(Duration::from_secs(60)).unwrap();
                    }
                },
                || {
                    high_endpoint.exchange(
                        Connection::admit(high_listener.accept().unwrap().0, Scope::Loopback)
                            .unwrap(),
                        Side::Responder,
                        &mut Repair::default(),
                    )
                },
            )
        });
        let winning_initiator = scope.spawn(move || {
            let mut waiting = Some(waiting);
            observe_peer_cache(
                move |stage| {
                    if matches!(stage, PeerCacheStage::Waiting)
                        && let Some(waiting) = waiting.take()
                    {
                        waiting.send(()).unwrap();
                    }
                },
                || {
                    low_endpoint.exchange(
                        Connection::connect(high_address, Scope::Loopback, Duration::from_secs(60))
                            .unwrap(),
                        Side::Initiator,
                        &mut Repair::default(),
                    )
                },
            )
        });
        let losing_initiator = scope.spawn(move || {
            high_endpoint.exchange(
                Connection::connect(low_address, Scope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            )
        });
        let rejection = losing_initiator.join().unwrap().unwrap_err();
        assert!(rejection.to_string().contains("active repair round"));
        assert!(losing_responder.join().unwrap().is_err());
        let initiated = winning_initiator.join().unwrap().unwrap();
        let responded = winning_responder.join().unwrap().unwrap();
        assert_eq!(initiated.sent + responded.sent, 2);
        assert_eq!(initiated.received + responded.received, 2);
    });
    let expected = BTreeSet::from([lab.genesis.id(), update.id()]);
    for n in 0..2 {
        assert_eq!(
            lab.journal(n).inventory().copied().collect::<BTreeSet<_>>(),
            expected
        );
    }
}

#[test]
fn a_session_builds_the_same_endpoint_without_reopening_its_locked_journal() {
    use crate::table::{Session, tests::Lab};
    use aterm_secrets::commons::roster::Role;
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    for n in 0..2 {
        let bootstrap = lab.bootstrap(n);
        let legacy = Endpoint::from_bootstrap(&bootstrap)
            .unwrap()
            .unwrap()
            .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config"))
            .unwrap();
        let admission = Instant::now() + Duration::from_secs(2);
        let bounded = Endpoint::from_bootstrap_until(&bootstrap, admission)
            .unwrap()
            .unwrap()
            .with_commons_until(
                lab.paths[n].clone(),
                lab.paths[n].home.join("config"),
                admission,
            )
            .unwrap();
        assert_eq!(bounded.hello, legacy.hello);
        assert_eq!(bounded.actor, legacy.actor);
        assert!(bounded.application.is_some());
        let session = Session::open(&bootstrap).unwrap();
        let foreign_home = aterm_tempfile::tempdir().unwrap();
        let mut foreign_trust = bootstrap.trust();
        foreign_trust.network[0] ^= 1;
        let foreign = Journal::open(foreign_home.path(), foreign_trust).unwrap();
        assert!(bootstrap.witness_in(&foreign).is_err());
        let endpoint = Endpoint::from_session(&session)
            .unwrap()
            .with_commons_from_session(
                &session,
                lab.paths[n].clone(),
                lab.paths[n].home.join("config"),
            )
            .unwrap();
        assert_eq!(endpoint.hello, legacy.hello);
        assert_eq!(endpoint.actor, legacy.actor);
        assert_eq!(endpoint.trust, legacy.trust);
        assert_eq!(endpoint.dir, legacy.dir);
        assert_eq!(endpoint.identity.key(), legacy.identity.key());
        assert!(endpoint.application.is_some());
        assert!(!lab.paths[n].dir.join("commons/tables").exists());
        assert!(
            Endpoint::from_session(&session)
                .unwrap()
                .with_commons_from_session(
                    &session,
                    lab.paths[1 - n].clone(),
                    lab.paths[n].home.join("config")
                )
                .is_err(),
            "a checked session must not authorize another custody path"
        );
        // Success above while this session still owns the journal is the
        // negative control against constructors that try to acquire it again.
        drop(session);
    }
}

#[test]
fn bounded_pending_setup_preserves_enrollment_identity_and_refuses_an_enrolled_device() {
    let lab = crate::control::enrollment::tests::Lab::new(true);
    let bootstrap = lab.bootstrap(1);
    let admission = Instant::now() + Duration::from_secs(2);
    assert!(
        Endpoint::from_bootstrap_until(&bootstrap, admission)
            .unwrap()
            .is_none()
    );
    let endpoint =
        Endpoint::pending_from_bootstrap_until(&bootstrap, lab.paths[1].clone(), admission)
            .unwrap();
    let legacy = lab.pending();
    assert!(endpoint.pending());
    assert_eq!(endpoint.hello, legacy.hello);
    assert_eq!(endpoint.actor, legacy.actor);
    assert!(endpoint.application.is_none());
    assert!(crate::table::Session::open(&bootstrap).is_err());
    assert!(
        Endpoint::pending_from_bootstrap_until(
            &lab.bootstrap(0),
            lab.paths[0].clone(),
            Instant::now() + Duration::from_secs(2),
        )
        .is_err()
    );
}

const ROOT: [u8; 32] = [17; 32];
const NETWORK: [u8; 16] = [9; 16];

#[test]
fn canonical_foundation_grants_no_channel_but_competing_signed_members_can_repair() {
    let root = aterm_tempfile::tempdir().unwrap();
    let foundation = Node::foundation(crate::crypto::public(&ROOT).unwrap()).unwrap();
    let trust = Trust {
        network: foundation.network(),
        authority: foundation.authority(),
        genesis: foundation.id(),
    };
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let requests: [Request; 2] = std::array::from_fn(|n| {
        Request::create(
            trust.network,
            trust.authority,
            [n as u8 + 1; 16],
            [n as u8 + 3; 16],
            &identities[n],
            &[n as u8 + 31; 32],
        )
        .unwrap()
    });
    let branches: [Node; 2] = std::array::from_fn(|n| {
        Node::test_seal(
            &ROOT,
            trust.network,
            Edit::Update {
                parent: foundation.id(),
                hosts: &[],
                payload: b"transport witness",
            },
            &[identities[n].recipient()],
            1,
            &[Fact::Bind {
                binding: requests[n].binding().clone(),
            }],
        )
        .unwrap()
    });
    let paths = [root.path().join("a"), root.path().join("b")];
    for n in 0..2 {
        let mut journal = Journal::open(&paths[n], trust).unwrap();
        journal.ingest(foundation.bytes()).unwrap();
        journal.ingest(branches[n].bytes()).unwrap();
        assert!(
            Endpoint::configured(
                &paths[n],
                trust,
                &requests[n],
                foundation.bytes(),
                &[n as u8 + 31; 32]
            )
            .is_err()
        );
    }
    let endpoints: [Endpoint; 2] = std::array::from_fn(|n| {
        Endpoint::new(
            &paths[n],
            trust,
            &requests[n],
            branches[n].bytes(),
            &[n as u8 + 31; 32],
        )
        .unwrap()
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            endpoints[1]
                .exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut Repair::default(),
                )
                .unwrap()
        });
        endpoints[0]
            .exchange(
                Connection::connect(addr, Scope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            )
            .unwrap();
        server.join().unwrap();
    });
    for path in &paths {
        let journal = Journal::open_existing(path, trust).unwrap();
        assert!(matches!(journal.state(), State::Conflict { heads } if heads.len() == 2));
        assert_eq!(journal.inventory().count(), 3);
        assert!(journal.current(&identities[0]).unwrap().is_none());
    }
}

#[test]
fn established_repair_storage_cannot_reinitialize_after_directory_loss() {
    let lab = Lab::new();
    lab.journal(0).ingest(lab.genesis.bytes()).unwrap();
    let path = lab.path(0);
    std::fs::rename(&path, lab.root.path().join("preserved-control")).unwrap();
    let disk = Disk {
        dir: &path,
        trust: lab.trust,
    };
    assert!(disk.inventory().is_err());
    assert!(disk.batch(&[lab.genesis.id()]).is_err());
    assert!(disk.retain(&[lab.genesis.bytes().to_vec()]).is_err());
    assert!(!path.join("control.astream").exists());
}

#[test]
fn control_endpoint_uses_the_secrets_backed_bootstrap_identity() {
    use aterm_secrets::{
        client::{self, Paths},
        commons::{
            self,
            binding::Pending,
            bootstrap::{Bootstrap, Descriptor, Device},
            roster::{Enrollment, Role},
        },
        text::Audience,
    };
    let home = aterm_tempfile::tempdir().unwrap();
    let source = home.path().join("test.vault");
    let paths = Paths::for_home(&home.path().join("m1"));
    client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut document = admin.doc.clone();
    document
        .set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            commons::ENTRY,
            b"pending",
        )
        .unwrap();
    client::admin_publish(&paths, &admin, &document).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let device = Device::open(&paths).unwrap().unwrap();
    let request = device
        .request(NETWORK, admin.commons_authority().unwrap())
        .unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, NETWORK, *request.binding().authority()).unwrap();
    let possession = pending
        .verify(&device.answer(&request, &challenge).unwrap())
        .unwrap();
    let genesis = admin
        .prepare_commons_genesis(
            NETWORK,
            vec![Enrollment::new("m1", possession, Role::Writer)],
        )
        .unwrap();
    let descriptor = Descriptor::from_genesis(genesis).unwrap();
    document
        .set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            commons::ENTRY,
            &descriptor.bytes(),
        )
        .unwrap();
    client::admin_publish(&paths, &admin, &document).unwrap();
    let bootstrap = Bootstrap::cached(&paths).unwrap().unwrap();
    let endpoint = Endpoint::from_bootstrap(&bootstrap).unwrap().unwrap();
    assert_eq!(endpoint.actor, request.binding().actor());
    assert_eq!(endpoint.trust, descriptor.trust());
    std::fs::rename(
        bootstrap.journal_dir(),
        home.path().join("preserved-control"),
    )
    .unwrap();
    assert!(Endpoint::from_bootstrap(&bootstrap).is_err());
}

struct Lab {
    root: aterm_tempfile::TempDir,
    trust: Trust,
    requests: [Request; 2],
    genesis: Node,
    identities: [Identity; 2],
}
impl Lab {
    fn new() -> Self {
        let authority = crate::crypto::public(&ROOT).unwrap();
        let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
        let requests = std::array::from_fn(|n| {
            Request::create(
                NETWORK,
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
            .map(|request| Fact::Bind {
                binding: request.binding().clone(),
            })
            .collect();
        // B cannot decrypt even the genesis. Transport must still recover it.
        let genesis = Node::test_seal(
            &ROOT,
            NETWORK,
            Edit::Create {
                hosts: &[],
                payload: b"sealed-audience-only",
            },
            &[identities[0].recipient()],
            0,
            &facts,
        )
        .unwrap();
        let root = aterm_tempfile::tempdir().unwrap();
        let trust = Trust {
            network: NETWORK,
            authority,
            genesis: genesis.id(),
        };
        Self {
            root,
            trust,
            requests,
            genesis,
            identities,
        }
    }
    fn path(&self, n: usize) -> PathBuf {
        self.root.path().join(n.to_string())
    }
    fn journal(&self, n: usize) -> Journal {
        Journal::open(&self.path(n), self.trust).unwrap()
    }
    fn endpoint(&self, n: usize) -> Endpoint {
        Endpoint::new(
            &self.path(n),
            self.trust,
            &self.requests[n],
            self.genesis.bytes(),
            &[n as u8 + 31; 32],
        )
        .unwrap()
    }
    fn update(&self, parent: Digest, payload: &[u8], facts: &[Fact]) -> Node {
        Node::test_seal(
            &ROOT,
            NETWORK,
            Edit::Update {
                parent,
                hosts: &[],
                payload,
            },
            &[self.identities[0].recipient()],
            0,
            facts,
        )
        .unwrap()
    }
    fn round(&self, a: &mut Repair, b: &mut Repair) -> (Report, Report) {
        let ea = self.endpoint(0);
        let eb = self.endpoint(1);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::scope(|scope| {
            let server = scope.spawn(move || {
                eb.exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    b,
                )
                .unwrap()
            });
            let report = ea
                .exchange(
                    Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN)
                        .unwrap(),
                    Side::Initiator,
                    a,
                )
                .unwrap();
            (report, server.join().unwrap())
        })
    }
}

#[test]
fn tls_repairs_retirement_without_any_epoch_key_or_recipient_access() {
    let lab = Lab::new();
    assert!(lab.genesis.open(&lab.identities[1]).is_err());
    let retired = lab.update(
        lab.genesis.id(),
        b"fresh-key-remains-sealed",
        &[Fact::Retire {
            actor: lab.requests[1].binding().actor(),
        }],
    );
    {
        let mut journal = lab.journal(0);
        journal.ingest(lab.genesis.bytes()).unwrap();
        journal.ingest(retired.bytes()).unwrap();
    }
    let (mut a, mut b) = (Repair::default(), Repair::default());
    let (sent, received) = lab.round(&mut a, &mut b);
    assert_eq!(a.phase(), Phase::ControlComplete);
    assert_eq!(b.phase(), Phase::ControlComplete);
    assert_eq!(sent.sent, 2);
    assert_eq!(received.received, 2);
    let journal = lab.journal(1);
    assert_eq!(journal.inventory().count(), 2);
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_recorded_device(lab.requests[1].binding())
            .is_err()
    );
    assert!(journal.current(&lab.identities[1]).is_err());
    drop(journal);
    assert_eq!(
        lab.round(&mut a, &mut b),
        (Report::default(), Report::default())
    );
    assert!(!lab.path(1).join("database.json").exists());
}

#[test]
fn tls_preserves_divergent_branches_and_repairs_missing_parents_across_pages() {
    let lab = Lab::new();
    let mut last = lab.genesis.id();
    let mut nodes = vec![lab.genesis.clone()];
    for n in 0..RECORDS_PER_ROUND * 2 + 3 {
        let node = lab.update(last, &[n as u8], &[]);
        last = node.id();
        nodes.push(node);
    }
    let fork = lab.update(lab.genesis.id(), b"independent-offline-branch", &[]);
    {
        let mut journal = lab.journal(0);
        for node in &nodes {
            journal.ingest(node.bytes()).unwrap();
        }
    }
    {
        let mut journal = lab.journal(1);
        journal.ingest(lab.genesis.bytes()).unwrap();
        journal.ingest(fork.bytes()).unwrap();
    }
    let (mut a, mut b) = (Repair::default(), Repair::default());
    for _ in 0..4 {
        lab.round(&mut a, &mut b);
    }
    for n in 0..2 {
        let journal = lab.journal(n);
        assert_eq!(journal.inventory().count(), nodes.len() + 1);
        assert!(matches!(
            journal.state(),
            aterm_secrets::commons::journal::State::Conflict { .. }
        ));
    }
    let mut heads = [last, fork.id()];
    heads.sort();
    let recovery = Node::test_seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &heads,
            selected: last,
        },
        &[],
        0,
        &[],
    )
    .unwrap();
    lab.journal(1).ingest(recovery.bytes()).unwrap();
    lab.round(&mut a, &mut b);
    for n in 0..2 {
        assert!(matches!(lab.journal(n).state(),
            aterm_secrets::commons::journal::State::Ready { head, basis }
                if head == recovery.id() && basis == last));
    }
}

#[test]
fn witness_and_channel_proof_must_name_the_same_exact_pinned_device() {
    let lab = Lab::new();
    let endpoint = lab.endpoint(1);
    let identity = &endpoint.identity;
    let proof = NodeProof {
        node: identity.node().into(),
        key: identity.key(),
    };
    let own = lab.requests[0].binding().actor();
    assert!(authenticate(lab.trust, own, Some(&proof), &endpoint.hello).is_ok());
    assert!(authenticate(lab.trust, own, None, &endpoint.hello).is_err());
    assert!(authenticate(lab.trust, endpoint.actor, Some(&proof), &endpoint.hello).is_err());
    let impostor = lab.endpoint(0);
    let wrong = NodeProof {
        node: proof.node.clone(),
        key: impostor.identity.key(),
    };
    assert!(authenticate(lab.trust, own, Some(&wrong), &endpoint.hello).is_err());
    let wrong = NodeProof {
        node: "aa".repeat(16),
        key: proof.key,
    };
    assert!(authenticate(lab.trust, own, Some(&wrong), &endpoint.hello).is_err());
    for at in [0, 4, 20, 52, 84, HELLO_FIXED, endpoint.hello.len() - 1] {
        let mut changed = endpoint.hello.clone();
        changed[at] ^= 1;
        assert!(authenticate(lab.trust, own, Some(&proof), &changed).is_err());
    }
    assert!(
        Endpoint::new(
            &lab.path(2),
            lab.trust,
            &lab.requests[1],
            lab.genesis.bytes(),
            &[99; 32]
        )
        .is_err()
    );
    let other_genesis = Node::test_seal(
        &ROOT,
        NETWORK,
        Edit::Create {
            hosts: &[],
            payload: b"competing-network-root",
        },
        &[lab.identities[0].recipient()],
        0,
        lab.genesis.facts(),
    )
    .unwrap();
    assert!(check_witness(lab.trust, &lab.requests[1], other_genesis.bytes()).is_err());
    let unapproved = lab.update(lab.genesis.id(), b"no-admission", &[]);
    assert!(check_witness(lab.trust, &lab.requests[1], unapproved.bytes()).is_err());
}

#[test]
fn real_tls_rejects_a_copied_witness_without_its_writer_key() {
    let lab = Lab::new();
    let server = lab.endpoint(0);
    let mut impostor = lab.endpoint(1);
    impostor.identity = NodeIdentity::from_seed(
        &aterm_codec::hex::encode_lower(lab.requests[1].binding().incarnation()),
        &[99; 32],
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::scope(|scope| {
        let served = scope.spawn(|| {
            server.exchange(
                Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                Side::Responder,
                &mut Repair::default(),
            )
        });
        assert!(
            impostor
                .exchange(
                    Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN)
                        .unwrap(),
                    Side::Initiator,
                    &mut Repair::default()
                )
                .is_err()
        );
        assert!(served.join().unwrap().is_err());
    });
    assert_eq!(lab.journal(0).inventory().count(), 0);
    assert_eq!(lab.journal(1).inventory().count(), 0);
}

#[test]
fn truncated_native_tls_exchanges_report_the_current_local_phase() {
    let lab = Lab::new();
    for n in 0..2 {
        lab.journal(n).ingest(lab.genesis.bytes()).unwrap();
    }
    for cut in [
        Phase::Membership,
        Phase::InventoryReceive,
        Phase::BatchReceive,
    ] {
        let client = lab.endpoint(0);
        let peer = lab.endpoint(1);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut domain = b"aterm/commons/control-transport/1\0".to_vec();
        domain.extend_from_slice(&lab.trust.network);
        domain.extend_from_slice(&lab.trust.authority);
        domain.extend_from_slice(&lab.trust.genesis);
        let discriminator = FleetSecret::new(&Sha256::digest(&domain)).unwrap();
        thread::scope(|scope| {
            let served = scope.spawn(|| {
                let connection =
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap();
                let (tcp, route) = connection.into_parts();
                tcp.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
                tcp.set_write_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut channel = fleet::server_handshake_as(
                    tcp,
                    &discriminator,
                    Some(&peer.identity),
                    FleetScope::Facts,
                    &[FleetOp::Database],
                    Duration::from_secs(60),
                )
                .unwrap();
                let stream = channel.transport.stream();
                receive_frame(stream, MAX_HELLO).unwrap();
                if cut == Phase::Membership {
                    stream
                        .write_all(&(peer.hello.len() as u32).to_le_bytes())
                        .unwrap();
                    stream
                        .write_all(&peer.hello[..peer.hello.len() / 2])
                        .unwrap();
                } else {
                    send_frame(stream, &peer.hello).unwrap();
                    receive_inventory(stream).unwrap();
                    if cut == Phase::BatchReceive {
                        send_inventory(stream, &BTreeSet::from([lab.genesis.id()])).unwrap();
                        assert_eq!(read_u32(stream).unwrap(), 0);
                    }
                    stream.write_all(&[0; 2]).unwrap();
                }
                stream.flush().unwrap();
                route.finish().unwrap();
            });
            let mut repair = Repair {
                phase: Phase::Complete,
                application: Some(crate::replica::Report::default()),
                ..Repair::default()
            };
            let result = client.exchange(
                Connection::connect(address, Scope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                &mut repair,
            );
            served.join().unwrap();
            assert!(result.is_err());
            assert_eq!(repair.phase(), cut);
            assert_eq!(
                repair.authenticated_actor(),
                (cut != Phase::Membership).then_some(peer.actor)
            );
            assert!(repair.application().is_none());
        });
    }
}

struct Script {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}
impl Read for Script {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for Script {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn offer(wires: &[Vec<u8>]) -> Script {
    let mut input = (wires.len() as u32).to_le_bytes().to_vec();
    for wire in wires {
        send_frame(&mut input, wire).unwrap();
    }
    Script {
        input: Cursor::new(input),
        output: Vec::new(),
    }
}

#[test]
fn failed_append_is_deferred_and_a_later_exact_retry_is_acknowledged() {
    let lab = Lab::new();
    let store = Disk {
        dir: &lab.path(0),
        trust: lab.trust,
    };
    let wire = lab.genesis.bytes().to_vec();
    let locked = lab.journal(0);
    let mut script = offer(std::slice::from_ref(&wire));
    assert_eq!(receive_batch(&mut script, &store).unwrap(), (0, 1));
    assert_eq!(script.output[32], 0);
    assert_eq!(locked.inventory().count(), 0);
    drop(locked);
    for _ in 0..2 {
        let mut script = offer(std::slice::from_ref(&wire));
        assert_eq!(receive_batch(&mut script, &store).unwrap(), (1, 0));
        assert_eq!(
            script.output,
            [Sha256::digest(&wire).as_slice(), &[1]].concat()
        );
    }
    assert_eq!(lab.journal(0).inventory().count(), 1);
}

#[test]
fn reads_and_known_records_need_no_writer_and_new_records_still_do() {
    let lab = Lab::new();
    let path = lab.path(0);
    lab.journal(0).ingest(lab.genesis.bytes()).unwrap();
    let store = Disk {
        dir: &path,
        trust: lab.trust,
    };
    let known = lab.genesis.bytes().to_vec();
    let fresh = lab.update(lab.genesis.id(), b"next", &[]);
    {
        // Acknowledging records already held opens no writer: the one flush is
        // the directory barrier behind the acknowledgement.
        let recording = aterm_durable::seam::Recording::start();
        assert_eq!(store.retain(std::slice::from_ref(&known)).unwrap(), [true]);
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable]);
    }
    let held = lab.journal(0);
    assert_eq!(
        store.inventory().unwrap(),
        BTreeSet::from([lab.genesis.id()])
    );
    assert_eq!(
        store.batch(&[lab.genesis.id()]).unwrap(),
        std::slice::from_ref(&known)
    );
    assert_eq!(
        store.retain(&[known.clone(), b"garbage".to_vec()]).unwrap(),
        [true, false],
        "records already held are acknowledged beside the writer"
    );
    {
        // An acknowledgement claims durability: a failed directory barrier
        // withholds it rather than trust a checkpoint rename not yet durable.
        let _failing = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO);
        assert!(store.retain(std::slice::from_ref(&known)).is_err());
    }
    assert_eq!(
        store
            .retain(&[known.clone(), fresh.bytes().to_vec()])
            .unwrap(),
        [true, false],
        "a new record needs the writer; while it is busy, the peer offers it again"
    );
    drop(held);
    assert_eq!(
        store.retain(&[known, fresh.bytes().to_vec()]).unwrap(),
        [true, true]
    );
    assert_eq!(store.inventory().unwrap().len(), 2);
}

#[test]
fn network_reads_and_writes_never_own_the_journal_lock() {
    struct Unlocked<'a> {
        script: Script,
        dir: &'a Path,
        trust: Trust,
    }
    impl Read for Unlocked<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            drop(Journal::open(self.dir, self.trust).expect("network read held journal lock"));
            self.script.read(bytes)
        }
    }
    impl Write for Unlocked<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            drop(Journal::open(self.dir, self.trust).expect("network write held journal lock"));
            self.script.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.script.flush()
        }
    }
    let lab = Lab::new();
    let path = lab.path(0);
    let store = Disk {
        dir: &path,
        trust: lab.trust,
    };
    let mut stream = Unlocked {
        script: offer(&[lab.genesis.bytes().to_vec()]),
        dir: &path,
        trust: lab.trust,
    };
    assert_eq!(receive_batch(&mut stream, &store).unwrap(), (1, 0));
    assert_eq!(lab.journal(0).inventory().count(), 1);
}

#[test]
fn missing_channel_proof_and_invalid_signatures_do_not_touch_storage() {
    let lab = Lab::new();
    let endpoint = lab.endpoint(1);
    for side in [Side::Initiator, Side::Responder] {
        let mut stream = offer(&[]);
        assert!(
            endpoint
                .connected(&mut stream, None, side, &mut Repair::default())
                .is_err()
        );
        assert!(stream.output.is_empty());
        assert_eq!(stream.input.position(), 0);
    }
    let path = lab.path(0);
    assert!(!path.exists());
    let mut corrupt = lab.genesis.bytes().to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    let mut stream = offer(&[corrupt]);
    assert_eq!(
        receive_batch(
            &mut stream,
            &Disk {
                dir: &path,
                trust: lab.trust
            }
        )
        .unwrap(),
        (0, 1)
    );
    assert!(
        !path.exists(),
        "unauthenticated bytes must not cause a journal replay"
    );
}

/// Real signed witnesses and TLS-proof identities for the admission model.
pub fn admission_transition(
    state: &aterm_spec::interp::State,
    action: &str,
    bypass: bool,
) -> aterm_spec::interp::State {
    let lab = Lab::new();
    let mut endpoint = lab.endpoint(1);
    let mut proof = Some(NodeProof {
        node: endpoint.identity.node().into(),
        key: endpoint.identity.key(),
    });
    let scenario = match action {
        "Approved" => 1,
        "MissingProof" => {
            proof = None;
            2
        }
        "CopiedWitness" => {
            proof.as_mut().unwrap().key = lab.endpoint(0).identity.key();
            3
        }
        "ForeignPins" => {
            endpoint.hello[52] ^= 1;
            4
        }
        "ForeignGenesis" => {
            let genesis = Node::test_seal(
                &ROOT,
                NETWORK,
                Edit::Create {
                    hosts: &[],
                    payload: b"foreign-genesis",
                },
                &[lab.identities[0].recipient()],
                0,
                lab.genesis.facts(),
            )
            .unwrap();
            endpoint.hello.truncate(HELLO_FIXED);
            endpoint.hello.extend_from_slice(genesis.bytes());
            5
        }
        "Unapproved" => {
            let witness = lab.update(lab.genesis.id(), b"no-binding", &[]);
            endpoint.hello.truncate(HELLO_FIXED);
            endpoint.hello.extend_from_slice(witness.bytes());
            6
        }
        "ReflectedWriter" => {
            // A different incarnation of our own writer is still reflection.
            let request = Request::create(
                NETWORK,
                lab.trust.authority,
                [77; 16],
                [78; 16],
                &lab.identities[0],
                &[31; 32],
            )
            .unwrap();
            let witness = lab.update(
                lab.genesis.id(),
                b"same-key-new-incarnation",
                &[Fact::Bind {
                    binding: request.binding().clone(),
                }],
            );
            endpoint = Endpoint::new(
                &lab.path(2),
                lab.trust,
                &request,
                witness.bytes(),
                &[31; 32],
            )
            .unwrap();
            proof = Some(NodeProof {
                node: endpoint.identity.node().into(),
                key: endpoint.identity.key(),
            });
            7
        }
        _ => panic!("unknown control-admission action"),
    };
    let admitted = bypass
        || authenticate(
            lab.trust,
            lab.requests[0].binding().actor(),
            proof.as_ref(),
            &endpoint.hello,
        )
        .is_ok();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("admitted", i64::from(admitted));
    next
}

#[test]
fn control_admission_model_conforms_and_rejects_copied_witness_bypass() {
    let model = aterm_models_db::control_admission_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&admission_transition(&state, action.name, false))
                );
            }
        }
    }
    let state = std::collections::BTreeMap::from([("scenario", 0), ("admitted", 0)]);
    let bad = admission_transition(&state, "CopiedWitness", true);
    assert!(!model.successors("CopiedWitness", &state).contains(&bad));
    assert!(!model.check_invariant("OnlyApprovedAdmits", &bad));
}

#[test]
fn one_bad_record_does_not_block_other_signed_records_in_the_batch() {
    let lab = Lab::new();
    drop(lab.journal(0));
    let store = Disk {
        dir: &lab.path(0),
        trust: lab.trust,
    };
    let mut bad = lab.genesis.bytes().to_vec();
    *bad.last_mut().unwrap() ^= 1;
    let node = lab.update(lab.genesis.id(), b"independent-valid-arrival", &[]);
    let mut script = offer(&[bad, node.bytes().to_vec(), lab.genesis.bytes().to_vec()]);
    assert_eq!(receive_batch(&mut script, &store).unwrap(), (2, 1));
    assert_eq!(script.output[32], 0);
    assert_eq!(script.output[65], 1);
    assert_eq!(script.output[98], 1);
    assert_eq!(lab.journal(0).inventory().count(), 2);
}

#[test]
fn deferred_prefixes_and_changed_peers_cannot_strand_later_history() {
    let local: BTreeSet<_> = (0..45).map(|n| [n; 32]).collect();
    let remote = BTreeSet::new();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    for _ in 0..3 {
        let page = select(&local, &remote, cursor);
        assert_eq!(page.len(), RECORDS_PER_ROUND);
        cursor = page.last().copied();
        seen.extend(page);
    }
    assert_eq!(seen, local, "even if every offered record was deferred");
    assert!(select(&local, &local, cursor).is_empty());
    let lab = Lab::new();
    let mut repair = Repair::default();
    repair.prepare(lab.trust, lab.requests[1].binding().actor());
    repair.cursor = cursor;
    repair.prepare(lab.trust, lab.requests[1].binding().actor());
    assert_eq!(repair.cursor, cursor);
    repair.prepare(lab.trust, lab.requests[0].binding().actor());
    assert_eq!(repair.cursor, None);
    repair.cursor = cursor;
    let mut trust = lab.trust;
    trust.genesis[0] ^= 1;
    repair.prepare(trust, lab.requests[0].binding().actor());
    assert_eq!(repair.cursor, None);
}

#[test]
fn hostile_framing_is_bounded_and_receipts_require_exact_identity() {
    assert!(receive_frame(&mut Cursor::new(u32::MAX.to_le_bytes()), MAX_NODE).is_err());
    assert!(receive_inventory(&mut Cursor::new(u32::MAX.to_le_bytes())).is_err());
    for ids in [vec![[1; 32], [1; 32]], vec![[2; 32], [1; 32]]] {
        let mut bytes = 2_u32.to_le_bytes().to_vec();
        for id in ids {
            bytes.extend_from_slice(&id);
        }
        assert!(receive_inventory(&mut Cursor::new(bytes)).is_err());
    }
    let lab = Lab::new();
    let store = Disk {
        dir: &lab.path(0),
        trust: lab.trust,
    };
    let wire = lab.genesis.bytes().to_vec();
    let mut duplicate = offer(&[wire.clone(), wire.clone()]);
    assert!(receive_batch(&mut duplicate, &store).is_err());
    assert!(duplicate.output.is_empty());
    assert_eq!(lab.journal(0).inventory().count(), 0);
    let mut huge = offer(&[]);
    huge.input = Cursor::new(u32::MAX.to_le_bytes().to_vec());
    assert!(receive_batch(&mut huge, &store).is_err());
    let mut short = offer(std::slice::from_ref(&wire));
    short.input.get_mut().pop();
    assert!(receive_batch(&mut short, &store).is_err());
    assert_eq!(lab.journal(0).inventory().count(), 0);
    for response in [
        vec![0; 33],
        [Sha256::digest(&wire).as_slice(), &[2]].concat(),
    ] {
        let mut script = Script {
            input: Cursor::new(response),
            output: Vec::new(),
        };
        assert!(send_batch(&mut script, std::slice::from_ref(&wire)).is_err());
    }
    let ids: BTreeSet<_> = (0..MAX_NODES as u32)
        .map(|n| {
            let mut id = [0; 32];
            id[..4].copy_from_slice(&n.to_be_bytes());
            id
        })
        .collect();
    let mut bytes = Vec::new();
    send_inventory(&mut bytes, &ids).unwrap();
    assert_eq!(receive_inventory(&mut Cursor::new(bytes)).unwrap(), ids);
}

/// Execute the production page selection at every reachable abstract state.
pub fn repair_transition(
    state: &aterm_spec::interp::State,
    action: &str,
    freeze_on_refusal: bool,
) -> aterm_spec::interp::State {
    let count = RECORDS_PER_ROUND;
    let local: BTreeSet<_> = (0..count * 3).map(|n| [n as u8; 32]).collect();
    let mut remote = BTreeSet::new();
    if state["middle"] == 1 {
        remote.extend((count..count * 2).map(|n| [n as u8; 32]));
    }
    if state["last"] == 1 {
        remote.extend((count * 2..count * 3).map(|n| [n as u8; 32]));
    }
    let mut repair = Repair {
        peer: None,
        cursor: match state["position"] {
            0 => None,
            1 => Some([count as u8 - 1; 32]),
            2 => Some([count as u8 * 2 - 1; 32]),
            _ => unreachable!(),
        },
        ..Repair::default()
    };
    let old_cursor = repair.cursor;
    let page = repair.next(&local, &remote);
    let mut next = state.clone();
    next.insert("rounds", state["rounds"] + 1);
    match action {
        "RefusedFirst" => {
            assert_eq!(page, (0..count).map(|n| [n as u8; 32]).collect::<Vec<_>>());
            if freeze_on_refusal {
                repair.cursor = old_cursor;
            }
        }
        "DeliveredMiddle" => {
            next.insert(
                "middle",
                i64::from(
                    page == (count..count * 2)
                        .map(|n| [n as u8; 32])
                        .collect::<Vec<_>>(),
                ),
            );
        }
        "DeliveredLast" => {
            next.insert(
                "last",
                i64::from(
                    page == (count * 2..count * 3)
                        .map(|n| [n as u8; 32])
                        .collect::<Vec<_>>(),
                ),
            );
        }
        _ => panic!("unknown control-repair action"),
    }
    let position = match repair.cursor {
        Some(id) if id == [count as u8 - 1; 32] => 1,
        Some(id) if id == [count as u8 * 2 - 1; 32] => 2,
        _ => 0,
    };
    next.insert("position", position);
    next
}

#[test]
fn control_repair_model_conforms_and_rejects_freezing_after_deferral() {
    let model = aterm_models_db::control_repair_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = repair_transition(&state, action.name, false);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    let initial = std::collections::BTreeMap::from([
        ("position", 0),
        ("rounds", 0),
        ("middle", 0),
        ("last", 0),
    ]);
    let bad = repair_transition(&initial, "RefusedFirst", true);
    assert!(
        !model.successors("RefusedFirst", &initial).contains(&bad),
        "negative control must catch a cursor pinned to refused history"
    );
}

#[test]
fn either_direction_refuses_a_second_channel_for_the_same_peer_before_inventory() {
    let lab = Lab::new();
    for side in [Side::Initiator, Side::Responder] {
        let endpoint = lab.endpoint(1);
        let peer = lab.endpoint(0);
        let mut held = Repair::default();
        let lease = endpoint
            .peers
            .acquire_on(peer.actor, &mut held, true, Instant::now())
            .unwrap();
        let proof = NodeProof {
            node: peer.identity.node().into(),
            key: peer.identity.key(),
        };
        let mut input = Vec::new();
        send_frame(&mut input, &peer.hello).unwrap();
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let mut repair = Repair::default();
        assert!(
            endpoint
                .connected(&mut stream, Some(&proof), side, &mut repair)
                .is_err()
        );
        assert_eq!(
            stream.output.len(),
            if side == Side::Initiator {
                4 + endpoint.hello.len()
            } else {
                0
            }
        );
        assert_eq!(repair.actor(), Some(peer.actor));
        assert_eq!(repair.phase(), Phase::PeerAdmission);
        drop(lease);
        drop(endpoint.peers.acquire(peer.actor, &mut held).unwrap());
    }
}

#[test]
fn failed_responder_hello_reports_current_peer_without_stale_application_success() {
    struct FailedReply(Cursor<Vec<u8>>);
    impl Read for FailedReply {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(bytes)
        }
    }
    impl Write for FailedReply {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "reply lost",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let lab = Lab::new();
    for cached in [false, true] {
        let endpoint = lab.endpoint(1);
        let peer = lab.endpoint(0);
        if cached {
            let mut saved = Repair::default();
            saved.prepare(lab.trust, peer.actor);
            saved.cursor = Some([9; 32]);
            saved.application = Some(crate::replica::Report::default());
            saved.phase = Phase::Complete;
            drop(endpoint.peers.acquire(peer.actor, &mut saved).unwrap());
        }
        let mut repair = Repair::default();
        repair.prepare(lab.trust, endpoint.actor);
        repair.cursor = Some([7; 32]);
        repair.application = Some(crate::replica::Report::default());
        repair.phase = Phase::Complete;
        let proof = NodeProof {
            node: peer.identity.node().into(),
            key: peer.identity.key(),
        };
        let mut bytes = (peer.hello.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&peer.hello);
        let mut stream = FailedReply(Cursor::new(bytes));
        assert!(
            endpoint
                .connected(&mut stream, Some(&proof), Side::Responder, &mut repair)
                .is_err()
        );
        assert_eq!(repair.actor(), Some(peer.actor));
        assert_eq!(repair.phase(), Phase::Membership);
        assert!(repair.application().is_none());
        assert_eq!(repair.cursor, if cached { Some([9; 32]) } else { None });
    }
}

#[test]
fn failed_pre_authentication_connected_round_clears_previous_round_state() {
    let lab = Lab::new();
    let endpoint = lab.endpoint(1);
    let peer = lab.endpoint(0);
    let mut repair = Repair::default();
    repair.prepare(lab.trust, peer.actor);
    repair.cursor = Some([7; 32]);
    repair.application = Some(crate::replica::Report::default());
    repair.application_error = Some("previous upgrade failed".into());
    repair.said = Said::Nothing;
    repair.phase = Phase::Complete;
    let mut stream = Script {
        input: Cursor::new(Vec::new()),
        output: Vec::new(),
    };
    let proof = NodeProof {
        node: peer.identity.node().into(),
        key: peer.identity.key(),
    };
    assert!(
        endpoint
            .connected(&mut stream, Some(&proof), Side::Responder, &mut repair)
            .is_err()
    );
    assert_eq!(
        repair.actor(),
        Some(peer.actor),
        "retain scheduling hint for peer"
    );
    assert_eq!(
        repair.authenticated_actor(),
        None,
        "a round that failed before authentication must not report current authentication"
    );
    assert_eq!(repair.phase(), Phase::Membership);
    assert_eq!(repair.said(), Said::Unknown);
    assert!(repair.application().is_none());
    assert!(repair.application_error().is_none());
    assert_eq!(repair.cursor, Some([7; 32]));
}

#[test]
fn commons_control_repairs_membership_records_and_tolerates_legacy_peers() {
    use aterm_secrets::client::{Paths, load_records, save_records};
    use aterm_secrets::membership_log::{Body, Entrant, Evidence, Log, Record, encode_records};

    let lab = Lab::new();
    {
        let mut j0 = lab.journal(0);
        j0.ingest(lab.genesis.bytes()).unwrap();
        let mut j1 = lab.journal(1);
        j1.ingest(lab.genesis.bytes()).unwrap();
    }
    let home0 = aterm_tempfile::tempdir().unwrap();
    let home1 = aterm_tempfile::tempdir().unwrap();
    let paths0 = Paths::for_home(home0.path());
    let paths1 = Paths::for_home(home1.path());

    let mk_entrant = |tag: u8, label: &str, net: [u8; 16], auth: [u8; 32]| {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let inc = [tag; 16];
        let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = aterm_secrets::commons::binding::Request::create(
            net, auth, machine, inc, &identity, &writer,
        )
        .unwrap();
        let actor = req.binding().actor();
        (
            writer,
            inc,
            actor,
            Entrant {
                request: req,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
        )
    };
    let (w1, inc1, m1, e1) = mk_entrant(1, "m1", lab.trust.network, lab.trust.authority);
    let (w2, inc2, m2, e2) = mk_entrant(2, "m2", lab.trust.network, lab.trust.authority);
    let (_, _, _, e3) = mk_entrant(3, "m3", lab.trust.network, lab.trust.authority);

    let genesis = Record::found(&[7; 32], vec![e1], 1_000).unwrap();
    let mut log0 = Log::new(genesis.clone()).unwrap();
    let admit_two = log0
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();
    log0.insert(admit_two.clone()).unwrap();
    // A record signed by `m2` whose id sorts before `admit_two` in `BTreeSet<Digest>` order,
    // plus an orphan waiting record missing its parent: `MembershipDisk::inventory` must not
    // offer the waiting record, and `retain_records_for_commons` must order `admit_two` ahead of
    // `name_two` so both land in one round.
    let name_two = (0..64)
        .map(|i| {
            log0.draft(
                &m2,
                1_010 + i,
                Body::Name {
                    name: format!("mac-2-{i}"),
                },
            )
            .sign(&w2, inc2)
            .unwrap()
        })
        .find(|r| r.id() < admit_two.id())
        .expect("one of 64 ids sorts before admit_two");
    let orphan_waiting = aterm_secrets::membership_log::Draft {
        network: *genesis.id(),
        sequence: 99,
        previous: Some([0x77; 32]),
        parents: vec![[0x77; 32]],
        authored_ms: 2_000,
        body: Body::Name {
            name: "orphan-waiting".to_owned(),
        },
    }
    .sign(&w1, inc1)
    .unwrap();
    let admit_three = Log::new(genesis.clone())
        .unwrap()
        .draft(
            &m1,
            1_002,
            Body::Admit {
                entrant: e3,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();

    save_records(
        &paths0,
        &encode_records([&genesis, &admit_two, &name_two, &orphan_waiting]),
    )
    .unwrap();
    save_records(&paths1, &encode_records([&genesis, &admit_three])).unwrap();

    // Negative control: a record founded for another Commons root is refused by `retain_records_for_commons`.
    let (_, _, _, foreign_e) = mk_entrant(9, "m9", [0xee; 16], lab.trust.authority);
    let foreign_genesis = Record::found(&[8; 32], vec![foreign_e], 1_000).unwrap();
    let home_foreign = aterm_tempfile::tempdir().unwrap();
    let paths_foreign = Paths::for_home(home_foreign.path());
    assert_eq!(
        aterm_secrets::client::retain_records_for_commons(
            &paths_foreign,
            lab.trust.network,
            lab.trust.authority,
            &[foreign_genesis.bytes().to_vec()],
        )
        .unwrap(),
        vec![false]
    );

    let ea = lab.endpoint(0).with_membership_paths(paths0.clone());
    let eb = lab.endpoint(1).with_membership_paths(paths1.clone());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut ra, mut rb) = (Repair::default(), Repair::default());
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            eb.exchange(
                Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                Side::Responder,
                &mut rb,
            )
            .unwrap()
        });
        ea.exchange(
            Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN).unwrap(),
            Side::Initiator,
            &mut ra,
        )
        .unwrap();
        server.join().unwrap();
    });

    assert_eq!(
        ra.membership(),
        Some(Report {
            sent: 2,
            received: 1,
            deferred: 0,
        })
    );
    assert_eq!(
        rb.membership(),
        Some(Report {
            sent: 1,
            received: 2,
            deferred: 0,
        })
    );
    let complete_union = BTreeSet::from([
        *genesis.id(),
        *admit_two.id(),
        *name_two.id(),
        *admit_three.id(),
    ]);
    assert_eq!(load_records(&paths1).unwrap().inventory(), complete_union);
    assert!(
        !load_records(&paths1).unwrap().contains(orphan_waiting.id()),
        "waiting records must not travel over Commons control repair"
    );

    // Compatibility with a `0.145` (`legacy = true`) peer: control repair still succeeds cleanly.
    let mut legacy_b = lab.endpoint(1);
    legacy_b.legacy = true;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut ra_leg, mut rb_leg) = (Repair::default(), Repair::default());
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            legacy_b
                .exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut rb_leg,
                )
                .unwrap()
        });
        ea.exchange(
            Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN).unwrap(),
            Side::Initiator,
            &mut ra_leg,
        )
        .unwrap();
        server.join().unwrap();
    });
    assert_eq!(ra_leg.membership(), None);
}

#[test]
fn two_islands_admit_and_remove_then_tables_continue_after_the_heal() {
    use crate::{accounts::proposals::Proposals, database::Change};
    use aterm_secrets::{
        client::{
            JoinOptions, JoinPassword, Paths, admin_cached, admin_commons_init, admin_open,
            admin_publish_approved, admit_member_in_log, create, join, load_records,
            machine_identity, remove_member_in_log, save_records, sync, test_sign_genesis,
        },
        commons::{
            binding::Pending,
            bootstrap::{Bootstrap, Device},
            journal::Journal,
        },
        membership_log::{Entrant, Evidence},
    };

    let join_local = |paths: &Paths, source: &str, host: &str| {
        join(
            paths,
            &JoinOptions {
                source: source.to_owned(),
                host: Some(host.to_owned()),
                interactive: false,
                wait: Duration::from_secs(2),
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .expect("join local vault");
    };
    let enroll_v1 = |owner_paths: &Paths, member_paths: &Paths| {
        sync(member_paths, None).expect("sync member");
        let owner = Bootstrap::required(owner_paths).expect("owner bootstrap");
        let peer = Bootstrap::required(member_paths).expect("peer bootstrap");
        let req = peer.request().expect("peer request");
        let (mut pending, ch) =
            Pending::issue(&req, owner.trust().network, owner.trust().authority).unwrap();
        let ans = peer.device().answer(&req, &ch).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin_cached(owner_paths)
            .unwrap()
            .enroll_commons_member(&owner, &mut journal, pending.verify(&ans).unwrap())
            .expect("v1 enroll");
        let mut peer_journal = Journal::open(peer.journal_dir(), peer.trust()).unwrap();
        let ids: Vec<_> = journal.inventory().copied().collect();
        for id in ids {
            let _ = peer_journal
                .ingest(journal.node(&id).unwrap().bytes())
                .unwrap();
        }
    };
    let prop = |key: &str| Change {
        collection: "accounts".into(),
        key: key.into(),
        value: Some(aterm_json::json!({ "provider": "claude", "method": "oauth" })),
    };
    let pair_commons = |a: &Paths, cfg_a: &Path, b: &Paths, cfg_b: &Path, rounds: usize| {
        let (mut ra, mut rb) = (Repair::default(), Repair::default());
        for _ in 0..rounds {
            let ba = Bootstrap::required(a).expect("bootstrap a");
            let bb = Bootstrap::required(b).expect("bootstrap b");
            let ea = Endpoint::from_bootstrap(&ba)
                .expect("endpoint a")
                .expect("witness a")
                .with_commons(a.clone(), cfg_a.to_path_buf())
                .expect("commons a");
            let eb = Endpoint::from_bootstrap(&bb)
                .expect("endpoint b")
                .expect("witness b")
                .with_commons(b.clone(), cfg_b.to_path_buf())
                .expect("commons b");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            thread::scope(|scope| {
                let server = scope.spawn(|| {
                    eb.exchange(
                        Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                        Side::Responder,
                        &mut rb,
                    )
                    .expect("responder exchange")
                });
                ea.exchange(
                    Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN)
                        .unwrap(),
                    Side::Initiator,
                    &mut ra,
                )
                .expect("initiator exchange");
                server.join().unwrap();
            });
            assert!(
                ra.application_error().is_none(),
                "initiator app error: {:?}",
                ra.application_error()
            );
            assert!(
                rb.application_error().is_none(),
                "responder app error: {:?}",
                rb.application_error()
            );
        }
    };

    // 1. Negative control: on the v1 authority-roster path (`!is_log_backed()`), independent
    //    admissions on two islands fork `Journal` into `State::Conflict` on heal and brick table
    //    sessions on both sides.
    let neg_root = aterm_tempfile::tempdir().unwrap();
    let v1_vault_a = neg_root.path().join("v1-a.vault");
    let v1_a = Paths::for_home(&neg_root.path().join("v1-a"));
    let v1_created = create(&v1_a, v1_vault_a.to_str().unwrap(), None, Some("m1")).unwrap();
    admin_commons_init(&v1_a).unwrap();
    let v1_pin_a = format!("{}#{}", v1_vault_a.display(), v1_created.signer);
    let v1_b = Paths::for_home(&neg_root.path().join("v1-b"));
    let v1_left = Paths::for_home(&neg_root.path().join("v1-left"));
    let v1_right = Paths::for_home(&neg_root.path().join("v1-right"));
    let rb_str = machine_identity(&v1_b).unwrap().recipient().to_bech32();
    let rl_str = machine_identity(&v1_left).unwrap().recipient().to_bech32();
    let rr_str = machine_identity(&v1_right).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&v1_a).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &rb_str).unwrap();
        doc.add_machine("m3", &rl_str).unwrap();
        doc.add_machine("m4", &rr_str).unwrap();
        doc.add_admin("m2").unwrap();
        admin_publish_approved(
            &v1_a,
            &admin,
            &doc,
            &[rb_str.clone(), rl_str.clone(), rr_str.clone()],
        )
        .unwrap();
    }
    join_local(&v1_b, &v1_pin_a, "m2");
    join_local(&v1_left, &v1_pin_a, "m3");
    join_local(&v1_right, &v1_pin_a, "m4");
    enroll_v1(&v1_a, &v1_b);
    let cfg_v1_a = neg_root.path().join("cfg-v1-a.toml");
    let cfg_v1_b = neg_root.path().join("cfg-v1-b.toml");
    pair_commons(&v1_a, &cfg_v1_a, &v1_b, &cfg_v1_b, 2);

    // Split: `v1_a` enrolls `v1_left`, while `v1_b` independently enrolls `v1_right`.
    enroll_v1(&v1_a, &v1_left);
    enroll_v1(&v1_b, &v1_right);
    {
        let ba = Bootstrap::required(&v1_a).unwrap();
        let bb = Bootstrap::required(&v1_b).unwrap();
        assert!(!ba.is_log_backed().unwrap());
        let ea = Endpoint::from_bootstrap(&ba).unwrap().unwrap();
        let eb = Endpoint::from_bootstrap(&bb).unwrap().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::scope(|scope| {
            let s = scope.spawn(|| {
                eb.exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut Repair::default(),
                )
                .unwrap()
            });
            ea.exchange(
                Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            )
            .unwrap();
            s.join().unwrap();
        });
        let ja = Journal::read_existing(ba.journal_dir(), ba.trust()).unwrap();
        let jb = Journal::read_existing(bb.journal_dir(), bb.trust()).unwrap();
        assert!(matches!(ja.state(), State::Conflict { .. }));
        assert!(matches!(jb.state(), State::Conflict { .. }));
        assert!(crate::table::Session::open(&ba).is_err());
        assert!(crate::table::Session::open(&bb).is_err());
    }

    // 2. Log-backed installation (`F18`): two partitioned islands admit and remove with zero
    //    authority-seed signatures after founding, heal over `ACX2`, and tables continue.
    let root = aterm_tempfile::tempdir().unwrap();
    let vault_file = root.path().join("f18.vault");
    let m1 = Paths::for_home(&root.path().join("m1"));
    let created = create(&m1, vault_file.to_str().unwrap(), None, Some("m1")).unwrap();
    admin_commons_init(&m1).unwrap();
    let pinned = format!("{}#{}", vault_file.display(), created.signer);

    let m2 = Paths::for_home(&root.path().join("m2"));
    let m3 = Paths::for_home(&root.path().join("m3"));
    let m4 = Paths::for_home(&root.path().join("m4"));
    let r2 = machine_identity(&m2).unwrap().recipient().to_bech32();
    let r3 = machine_identity(&m3).unwrap().recipient().to_bech32();
    let r4 = machine_identity(&m4).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &r2).unwrap();
        doc.add_machine("m3", &r3).unwrap();
        doc.add_machine("m4", &r4).unwrap();
        admin_publish_approved(&m1, &admin, &doc, &[r2.clone(), r3.clone(), r4.clone()]).unwrap();
    }
    for (p, h) in [(&m2, "m2"), (&m3, "m3"), (&m4, "m4")] {
        join_local(p, &pinned, h);
    }
    enroll_v1(&m1, &m2);
    enroll_v1(&m1, &m3);

    // Prepare a conflicting v1 node before founding so we can also prove `State::Conflict` in
    // `Journal` no longer blocks log-backed Commons sessions or tables.
    let b1_pre = Bootstrap::required(&m1).unwrap();
    let trust = b1_pre.trust();
    let alt_j_dir = root.path().join("alt-j");
    aterm_secrets::fsx::ensure_private_dir(&alt_j_dir).unwrap();
    for entry in std::fs::read_dir(b1_pre.journal_dir()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            aterm_secrets::fsx::write_atomic(
                &alt_j_dir.join(entry.file_name()),
                &std::fs::read(entry.path()).unwrap(),
                0o600,
            )
            .unwrap();
        }
    }
    let (fork_node_id, fork_node_wire) = {
        let b4 = Bootstrap::required(&m4).unwrap();
        let req4 = b4.request().unwrap();
        let (mut pending, ch) = Pending::issue(&req4, trust.network, trust.authority).unwrap();
        let ans = b4.device().answer(&req4, &ch).unwrap();
        let mut alt_j = Journal::open_existing(&alt_j_dir, trust).unwrap();
        let node = admin_cached(&m1)
            .unwrap()
            .enroll_commons_member(&b1_pre, &mut alt_j, pending.verify(&ans).unwrap())
            .unwrap();
        (node.id(), node.bytes().to_vec())
    };

    enroll_v1(&m1, &m4);
    for p in [&m2, &m3, &m4] {
        sync(p, None).unwrap();
    }

    let cfg1 = root.path().join("c1.toml");
    let cfg2 = root.path().join("c2.toml");
    let cfg3 = root.path().join("c3.toml");
    let cfg4 = root.path().join("c4.toml");
    let cfg5 = root.path().join("c5.toml");

    // Pre-Genesis v2 table operation sealed under the v1 authority `Journal` roster: proves a
    // log-admitted newcomer (`m5`) can receive and open an `ACHG1` historical handoff issued under
    // a log `Commons` epoch and read pre-Genesis v2 rows even when `Journal` later enters
    // `State::Conflict`.
    let p1 = Proposals::at(m1.clone());
    p1.initialize().unwrap();
    p1.write([9; 16], vec![prop("pre-genesis-v1-from-m1")])
        .unwrap();
    pair_commons(&m1, &cfg1, &m2, &cfg2, 2);

    let genesis_out = root.path().join("f18.genesis");
    test_sign_genesis(&m1, &genesis_out, &created.signer).expect("sign genesis");
    let seeded_records = load_records(&m1).unwrap().encode();
    for p in [&m2, &m3, &m4] {
        save_records(p, &seeded_records).unwrap();
    }

    // Pre-cut table causal chain: `m3` writes a row, and `m4` writes a row causally parenting on
    // `m3`'s row; sync all 4 founders before the partition.
    pair_commons(&m1, &cfg1, &m3, &cfg3, 3);
    Proposals::at(m3.clone())
        .write([10; 16], vec![prop("pre-cut-from-m3")])
        .unwrap();
    pair_commons(&m3, &cfg3, &m4, &cfg4, 3);
    Proposals::at(m4.clone())
        .write([11; 16], vec![prop("pre-cut-from-m4-after-m3")])
        .unwrap();
    pair_commons(&m4, &cfg4, &m1, &cfg1, 2);
    pair_commons(&m1, &cfg1, &m2, &cfg2, 3);
    pair_commons(&m1, &cfg1, &m3, &cfg3, 2);
    for p in [&m1, &m2, &m3, &m4] {
        assert_eq!(Proposals::at(p.clone()).snapshot().unwrap().cells.len(), 3);
    }

    let initial_journal_ids: BTreeSet<_> = {
        let b1 = Bootstrap::required(&m1).unwrap();
        Journal::read_existing(b1.journal_dir(), trust)
            .unwrap()
            .inventory()
            .copied()
            .collect()
    };

    // --- PARTITION ---
    // Island A (`{m1, m2}`): non-admin `m2` admits newcomer `m5` via `admit_member_in_log` (no
    // authority seed!). Both `m1` and `m5` write rows on Island A.
    let m5 = Paths::for_home(&root.path().join("m5"));
    aterm_secrets::client::configure_log_member(&m5, &pinned, "m5").unwrap();
    let _ = machine_identity(&m5).unwrap();
    let req5 = Device::open(&m5)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let entrant5 = Entrant {
        request: req5,
        node_key: [0x55; 32],
        name: "m5".to_owned(),
        label: "m5".to_owned(),
    };
    admit_member_in_log(&m2, entrant5, Evidence::Phrase).expect("non-admin m2 admits m5");
    save_records(&m5, &load_records(&m2).unwrap().encode()).unwrap();
    pair_commons(&m2, &cfg2, &m5, &cfg5, 3);
    pair_commons(&m2, &cfg2, &m1, &cfg1, 2);

    p1.write([21; 16], vec![prop("island-a-from-m1")]).unwrap();
    Proposals::at(m5.clone())
        .write([25; 16], vec![prop("island-a-from-newcomer-m5")])
        .unwrap();
    pair_commons(&m1, &cfg1, &m2, &cfg2, 2);
    pair_commons(&m2, &cfg2, &m5, &cfg5, 2);
    pair_commons(&m1, &cfg1, &m2, &cfg2, 2);

    // Island B (`{m3, m4}`): non-admin `m4` removes `m3` via `remove_member_in_log`!
    let actor_m3 = Bootstrap::required(&m3)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    remove_member_in_log(&m4, actor_m3).expect("non-admin m4 removes m3");

    // `m4` refuses removed `m3` at the `ACX2` handshake, even before `m3` learns of its removal!
    {
        let b3 = Bootstrap::required(&m3).unwrap();
        let b4 = Bootstrap::required(&m4).unwrap();
        let e3 = Endpoint::from_bootstrap(&b3)
            .unwrap()
            .unwrap()
            .with_commons(m3.clone(), cfg3.clone())
            .unwrap();
        let e4 = Endpoint::from_bootstrap(&b4)
            .unwrap()
            .unwrap()
            .with_commons(m4.clone(), cfg4.clone())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                e4.exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut Repair::default(),
                )
            });
            let _ = e3.exchange(
                Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            );
            let res4 = server.join().unwrap();
            assert!(
                res4.is_err(),
                "responder m4 must refuse removed m3 at ACX2 handshake"
            );
        });

        // Even if removed `m3` (before learning of its removal) signs a `Body::Admit` for a fresh
        // sybil Mac `m_sybil` on top of `m3`'s pre-removal log, `m4` combines `m_sybil`'s witness
        // with `m4`'s local log (which cuts `m3`'s post-removal `Admit`) and refuses `m_sybil` at
        // the `ACX2` handshake too!
        let m_sybil = Paths::for_home(&root.path().join("m-sybil"));
        aterm_secrets::client::configure_log_member(&m_sybil, &pinned, "m99").unwrap();
        let _ = machine_identity(&m_sybil).unwrap();
        let sybil_req = Device::open(&m_sybil)
            .unwrap()
            .unwrap()
            .request(trust.network, trust.authority)
            .unwrap();
        admit_member_in_log(
            &m3,
            Entrant {
                request: sybil_req.clone(),
                node_key: [0x99; 32],
                name: "m99".to_owned(),
                label: "m99".to_owned(),
            },
            Evidence::Phrase,
        )
        .expect("m3 signs Admit for m_sybil on its stale log");
        save_records(&m_sybil, &load_records(&m3).unwrap().encode()).unwrap();
        let b_sybil = Bootstrap::required(&m_sybil).unwrap();
        let cfg_sybil = root.path().join("c-sybil.toml");
        let e_sybil = Endpoint::from_bootstrap(&b_sybil)
            .unwrap()
            .unwrap()
            .with_commons(m_sybil.clone(), cfg_sybil)
            .unwrap();
        let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr2 = listener2.local_addr().unwrap();
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                e4.exchange(
                    Connection::admit(listener2.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut Repair::default(),
                )
            });
            let _ = e_sybil.exchange(
                Connection::connect(addr2, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            );
            let res4 = server.join().unwrap();
            assert!(
                res4.is_err(),
                "responder m4 must refuse sybil admitted by removed m3 at ACX2 handshake"
            );
        });
    }
    // Once `m3` receives the log records, its local custody, `Session`, and table writes are
    // refused too.
    save_records(&m3, &load_records(&m4).unwrap().encode()).unwrap();
    assert!(Bootstrap::cached(&m3).unwrap().is_none());
    assert!(Bootstrap::required(&m3).is_err());
    assert!(
        Proposals::at(m3.clone())
            .write([33; 16], vec![prop("forged-after-remove")])
            .is_err()
    );

    // Inject the v1 `State::Conflict` into `m4`'s `Journal` and verify log-backed `m4` STILL
    // continues writing to `Proposals` after removing `m3`!
    {
        let b4 = Bootstrap::required(&m4).unwrap();
        let mut j4 = Journal::open_existing(b4.journal_dir(), trust).unwrap();
        let _ = j4.ingest(&fork_node_wire).unwrap();
        assert!(matches!(j4.state(), State::Conflict { .. }));
    }
    Proposals::at(m4.clone())
        .write([24; 16], vec![prop("island-b-from-m4-after-remove")])
        .expect("m4 continues writing after removing m3 despite causal parent and Journal fork");

    // --- HEAL ---
    // Pair across the islands over `ACX2`: `sync_acx2_membership` unions `.records` and closes
    // newcomer `m5`'s `Wrap` gap on `m4`'s rotated `Commons` epoch before table repair!
    pair_commons(&m1, &cfg1, &m4, &cfg4, 2);
    pair_commons(&m1, &cfg1, &m2, &cfg2, 2);
    pair_commons(&m2, &cfg2, &m5, &cfg5, 2);
    pair_commons(&m4, &cfg4, &m5, &cfg5, 2);

    for p in [&m1, &m2, &m4, &m5] {
        assert_eq!(
            Proposals::at(p.clone()).snapshot().unwrap().cells.len(),
            6,
            "all 6 pre-Genesis, pre-heal, and split-island cells must be readable after heal"
        );
    }

    // Tables continue after the heal: both newcomer `m5` and `m4` write new post-heal rows and
    // converge across all active members `{m1, m2, m4, m5}`.
    Proposals::at(m5.clone())
        .write([35; 16], vec![prop("post-heal-from-m5")])
        .expect("newcomer m5 writes after heal under rotated Commons epoch");
    Proposals::at(m4.clone())
        .write([34; 16], vec![prop("post-heal-from-m4")])
        .expect("m4 writes after heal");

    pair_commons(&m5, &cfg5, &m2, &cfg2, 2);
    pair_commons(&m2, &cfg2, &m1, &cfg1, 2);
    pair_commons(&m1, &cfg1, &m4, &cfg4, 2);
    pair_commons(&m4, &cfg4, &m5, &cfg5, 2);
    pair_commons(&m2, &cfg2, &m5, &cfg5, 2);

    let expected = Proposals::at(m1.clone()).snapshot().unwrap();
    assert_eq!(expected.cells.len(), 8);
    assert_eq!(expected.operations.len(), 8);
    for p in [&m2, &m4, &m5] {
        let snap = Proposals::at(p.clone()).snapshot().unwrap();
        assert_eq!(snap.cells, expected.cells);
        assert_eq!(
            snap.operations
                .iter()
                .map(|o| o.header.id)
                .collect::<Vec<_>>(),
            expected
                .operations
                .iter()
                .map(|o| o.header.id)
                .collect::<Vec<_>>()
        );
    }
    assert!(Proposals::at(m3.clone()).snapshot().is_err());

    // Verify the authority seed signed ZERO new `ACN3` control nodes after founding (only the
    // pre-minted `fork_node_id` injected at `m4` was replicated into `m1`'s journal).
    let mut expected_journal_ids = initial_journal_ids;
    expected_journal_ids.insert(fork_node_id);
    let final_journal_ids: BTreeSet<_> = {
        let b1 = Bootstrap::required(&m1).unwrap();
        Journal::read_existing(b1.journal_dir(), trust)
            .unwrap()
            .inventory()
            .copied()
            .collect()
    };
    assert_eq!(final_journal_ids, expected_journal_ids);

    // Narrowing `aterm-commons-control-v1` on `m1` to exclude `m4` rotates `EpochAudience::Commons`
    // in `m1`'s membership log without a `Body::Remove` record; `m1` must refuse `m4`'s `ACX2`
    // handshake (`verify_peer_against_local_log`) even before `m4` receives the rotated epoch!
    let b4_before = Bootstrap::required(&m4).unwrap();
    let e4_before = Endpoint::from_bootstrap(&b4_before)
        .unwrap()
        .unwrap()
        .with_commons(m4.clone(), cfg4.clone())
        .unwrap();
    {
        let admin = admin_open(&m1).unwrap();
        let mut doc = admin.doc.clone();
        let ctrl_bytes = doc
            .entries()
            .into_iter()
            .find(|e| e.name == aterm_secrets::commons::ENTRY)
            .unwrap()
            .value
            .as_slice()
            .to_vec();
        doc.set_entry(
            &aterm_secrets::text::Audience::Hosts(vec!["m1".to_owned(), "m2".to_owned()]),
            aterm_secrets::commons::ENTRY,
            &ctrl_bytes,
        )
        .unwrap();
        aterm_secrets::client::admin_publish(&m1, &admin, &doc).unwrap();
    }
    {
        let b1 = Bootstrap::required(&m1).unwrap();
        let e1 = Endpoint::from_bootstrap(&b1)
            .unwrap()
            .unwrap()
            .with_commons(m1.clone(), cfg1.clone())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                e1.exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut Repair::default(),
                )
            });
            let _ = e4_before.exchange(
                Connection::connect(addr, Scope::Loopback, aterm_spec::wait::MUST_HAPPEN).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            );
            let res1 = server.join().unwrap();
            assert!(
                res1.is_err(),
                "responder m1 must refuse Commons-excluded m4 at ACX2 handshake"
            );
        });
    }
}

#[test]
fn multi_round_membership_sync_sends_seats_and_parents_before_children_regardless_of_digest_order()
{
    use aterm_secrets::{
        age::Identity,
        client::{Paths, load_records, retain_records_for_commons, save_records},
        commons::binding::Request as BindRequest,
        membership_log::{Body, Entrant, Evidence, Founder, Log, Record, Root, encode_records},
    };
    use std::os::unix::net::UnixStream;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let sender = Paths::for_home(&tmp.path().join("sender"));
    let receiver = Paths::for_home(&tmp.path().join("receiver"));
    let network = [0x41; 16];
    let authority = [0x42; 32];
    let root = Root { network, authority };

    let id1 = Identity::from_secret_bytes([0x11; 32]);
    let w1 = [0x21; 32];
    let inc1 = [0x31; 16];
    let req1 = BindRequest::create(network, authority, [1; 16], inc1, &id1, &w1).unwrap();
    let m1 = req1.binding().actor();
    let genesis = Record::genesis(
        &[0x99; 32],
        root,
        vec![Founder {
            recipient: id1.recipient(),
            binding: Some(req1.binding().clone()),
            node_key: Some([0x51; 32]),
            name: "m1".into(),
            label: "m1".into(),
        }],
        1_000,
    )
    .unwrap();
    let trust = Trust {
        network,
        authority,
        genesis: *genesis.id(),
    };

    let id2 = Identity::from_secret_bytes([0x12; 32]);
    let w2 = [0x22; 32];
    let inc2 = [0x32; 16];
    let req2 = BindRequest::create(network, authority, [2; 16], inc2, &id2, &w2).unwrap();
    let m2 = req2.binding().actor();

    // Pick `admit_m2` with a high leading digest byte (`>= 0xF0`) so >= 16 child records signed by
    // `m2` sort strictly below `admit_m2.id()` in raw SHA-256 order.
    let base_log = Log::new(genesis.clone()).unwrap();
    let admit_m2 = (0u64..4_096)
        .map(|salt| {
            base_log
                .draft(
                    &m1,
                    2_000 + salt,
                    Body::Admit {
                        entrant: Entrant {
                            request: req2.clone(),
                            node_key: [0x52; 32],
                            name: "m2".into(),
                            label: "m2".into(),
                        },
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(&w1, inc1)
                .unwrap()
        })
        .find(|rec| rec.id()[0] >= 0xF0)
        .expect("high-digest admit");

    let mut log = Log::new(genesis.clone()).unwrap();
    log.insert(admit_m2.clone()).unwrap();
    let mut lower_children = 0usize;
    let mut seq = 0u64;
    while lower_children < RECORDS_PER_ROUND + 2 {
        seq += 1;
        let child = log
            .draft(
                &m2,
                10_000 + seq,
                Body::Name {
                    name: format!("m2-{seq}"),
                },
            )
            .sign(&w2, inc2)
            .unwrap();
        if child.id() < admit_m2.id() {
            lower_children += 1;
        }
        log.insert(child).unwrap();
    }

    let sender_records: Vec<Record> = log.records().cloned().collect();
    save_records(&sender, &encode_records(&sender_records)).unwrap();
    save_records(&receiver, &encode_records(std::slice::from_ref(&genesis))).unwrap();

    let sender_disk = MembershipDisk::new(&sender, trust);
    let receiver_disk = MembershipDisk::new(&receiver, trust);

    // Negative control: raw digest selection (`select`) chooses 16 `m2`-signed child records
    // ahead of `admit_m2`, and `retain_records_for_commons` rejects every single one because `m2`
    // holds no seat on `receiver` yet (`enforce_seats`).
    let local_inv = sender_disk.inventory().unwrap();
    let remote_inv = receiver_disk.inventory().unwrap();
    let digest_ids = select(&local_inv, &remote_inv, None);
    assert_eq!(digest_ids.len(), RECORDS_PER_ROUND);
    assert!(
        !digest_ids.contains(admit_m2.id()),
        "negative control requires admit_m2 to sort after the first 16 child digests"
    );
    let digest_wires = sender_disk.batch(&digest_ids).unwrap();
    let neg_dir = Paths::for_home(&tmp.path().join("neg-receiver"));
    save_records(&neg_dir, &encode_records(std::slice::from_ref(&genesis))).unwrap();
    let neg_acks = retain_records_for_commons(&neg_dir, network, authority, &digest_wires).unwrap();
    assert!(
        neg_acks.iter().all(|&ack| !ack),
        "without causal selection, round 1 rejects all 16 seatless child records"
    );

    // Positive proof: `exchange_membership_round` uses `MembershipDisk::select_missing` so round 1
    // sends `admit_m2` ahead of `m2`'s children (`sent == 16` acknowledged on the first try), and
    // every remaining record lands cleanly in the minimum `ceil(unsent / 16)` rounds.
    let missing_total = local_inv.difference(&remote_inv).count();
    let expected_rounds = missing_total.div_ceil(RECORDS_PER_ROUND);
    let (mut cur_a, mut cur_b) = (None, None);
    let mut rounds = 0usize;
    loop {
        rounds += 1;
        let (mut sa, mut sb) = UnixStream::pair().unwrap();
        let ((rep_a, done_a), (rep_b, done_b)) = thread::scope(|s| {
            let responder = s.spawn(|| {
                exchange_membership_round(&mut sb, &receiver_disk, Side::Responder, &mut cur_b)
                    .unwrap()
            });
            let initiator =
                exchange_membership_round(&mut sa, &sender_disk, Side::Initiator, &mut cur_a)
                    .unwrap();
            (initiator, responder.join().unwrap())
        });
        assert_eq!(rep_a.deferred, 0, "round {rounds} deferred records");
        assert_eq!(rep_b.deferred, 0, "round {rounds} responder deferred");
        assert_eq!(rep_a.sent, rep_b.received);
        if rounds == 1 {
            assert_eq!(rep_a.sent, RECORDS_PER_ROUND);
        }
        assert_eq!(done_a, done_b);
        if done_a {
            break;
        }
        assert!(rounds < expected_rounds + 1, "exceeded expected rounds");
    }
    assert_eq!(rounds, expected_rounds);
    assert_eq!(
        load_records(&receiver).unwrap().complete_inventory(),
        local_inv
    );
}

/// Every `ACX2` exchange ran at least one round in each of three passes, and each round asked
/// `MembershipDisk` three times (inventory, selection, batch), each a fresh read of the records
/// file that verifies every record and walks the log; both reconciles read it again. Converged
/// peers redial every few seconds with nothing to trade, so every Mac spent that work
/// continuously, growing with the log. Since 82fe1dc915 a converged exchange reads the file once
/// through `MembershipDisk` and reconciles once per side; this pins it.
/// NEGATIVE CONTROL: `MembershipDisk::with_store` reading the file at every call reads it twice
/// (main before 82fe1dc915 read it nine times and reconciled twice).
#[test]
fn a_converged_membership_exchange_reads_the_records_file_once_per_side() {
    use aterm_secrets::{
        age::Identity,
        client::{Paths, save_records},
        commons::binding::Request as BindRequest,
        membership_log::{Founder, Record, Root},
    };
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::Ordering;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let a = Paths::for_home(&tmp.path().join("a"));
    let b = Paths::for_home(&tmp.path().join("b"));
    let network = [0x43; 16];
    let authority = [0x44; 32];
    let id1 = Identity::from_secret_bytes([0x13; 32]);
    let req1 =
        BindRequest::create(network, authority, [3; 16], [0x33; 16], &id1, &[0x23; 32]).unwrap();
    let genesis = Record::genesis(
        &[0x98; 32],
        Root { network, authority },
        vec![Founder {
            recipient: id1.recipient(),
            binding: Some(req1.binding().clone()),
            node_key: Some([0x53; 32]),
            name: "m1".into(),
            label: "m1".into(),
        }],
        1_000,
    )
    .unwrap();
    let trust = Trust {
        network,
        authority,
        genesis: *genesis.id(),
    };
    save_records(&a, genesis.bytes()).unwrap();
    save_records(&b, genesis.bytes()).unwrap();

    let side = |stream: &mut UnixStream, paths: &Paths, side: Side| {
        let disk = MembershipDisk::new(paths, trust);
        let mut reconciles = 0usize;
        let report = sync_log_membership(stream, &disk, side, &mut None, &mut |_| {
            reconciles += 1;
            false
        })
        .unwrap();
        (report, disk.loads.load(Ordering::Relaxed), reconciles)
    };
    let (mut sa, mut sb) = UnixStream::pair().unwrap();
    let (initiator, responder) = thread::scope(|s| {
        let responder = s.spawn(|| side(&mut sb, &b, Side::Responder));
        let initiator = side(&mut sa, &a, Side::Initiator);
        (initiator, responder.join().unwrap())
    });
    for (report, loads, reconciles) in [initiator, responder] {
        assert_eq!((report.sent, report.received, report.deferred), (0, 0, 0));
        assert_eq!(
            loads, 1,
            "a converged exchange read the records file {loads} times"
        );
        assert_eq!(
            reconciles, 1,
            "a converged exchange reconciled {reconciles} times"
        );
    }
}
