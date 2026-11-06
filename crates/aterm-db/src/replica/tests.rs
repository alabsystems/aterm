// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    control::{Endpoint, Repair as PeerRepair},
    table::tests::Lab,
};
use aterm_net::lan::{Connection, Scope as LinkScope};
use aterm_secrets::commons::{
    journal::Journal,
    roster::{Grant, Role},
};
use std::{
    io::{self, Cursor},
    net::TcpListener,
    thread,
    time::Duration,
};

mod clock;
mod compatibility;
mod policy;
mod recovery;
pub(super) use policy::session_boundary;

#[test]
fn a_received_batch_keeps_its_first_local_refusal_and_durable_receipts() {
    let p = Pair::new(false, 1);
    let source = p.lab.bootstrap(0);
    let session = Session::observe(&source).unwrap();
    let peer = session.actor();
    let store = Store::observe(&session, &p.paths[0], p.scope).unwrap();
    let id = *store.inventory(&session).unwrap().next().unwrap();
    let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
    drop(store);
    drop(session);
    drop(source);

    let bad = b"private batch fixture value: not an operation";
    let mut incoming = 2u32.to_le_bytes().to_vec();
    crate::control::send_frame(&mut incoming, bad).unwrap();
    crate::control::send_frame(&mut incoming, &wire).unwrap();
    let replies = incoming.len();
    let mut stream = Cursor::new(incoming);
    let receiver = p.replica(1);
    let round = receiver.round(peer).unwrap();
    let mut repair = Repair::default();
    let mut report = Report::default();
    round
        .receive_batch(
            &mut stream,
            peer,
            p.scope.digest(),
            BatchKind::Operation,
            &mut repair,
            &mut report,
        )
        .unwrap();
    assert_eq!(report.received, 1);
    assert_eq!(report.deferred, 1);
    assert_eq!(p.count(1), 1);
    let receipts = &stream.get_ref()[replies..];
    assert_eq!(receipts.len(), 66);
    assert_eq!(&receipts[..32], &Sha256::digest(bad));
    assert_eq!(receipts[32], 0);
    assert_eq!(&receipts[33..65], &id);
    assert_eq!(receipts[65], 1);
    let why = repair
        .batch_deferral
        .as_ref()
        .expect("local refusal retained");
    assert_eq!(why.batch, "operations");
    assert_eq!(why.stage, "ingest");
    assert!(!why.reason.is_empty());
    assert!(!why.reason.contains("private batch fixture value"));
}

#[test]
fn a_received_batch_preserves_a_local_storage_refusal() {
    let p = Pair::new(false, 1);
    let source = p.lab.bootstrap(0);
    let session = Session::observe(&source).unwrap();
    let peer = session.actor();
    let store = Store::observe(&session, &p.paths[0], p.scope).unwrap();
    let id = *store.inventory(&session).unwrap().next().unwrap();
    let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
    drop(store);
    drop(session);
    drop(source);
    std::fs::rename(&p.paths[1], p.paths[1].with_extension("held")).unwrap();
    std::fs::write(&p.paths[1], b"not a table directory").unwrap();

    let mut incoming = 1u32.to_le_bytes().to_vec();
    crate::control::send_frame(&mut incoming, &wire).unwrap();
    let replies = incoming.len();
    let mut stream = Cursor::new(incoming);
    let receiver = p.replica(1);
    let round = receiver.round(peer).unwrap();
    let mut repair = Repair::default();
    let mut report = Report::default();
    round
        .receive_batch(
            &mut stream,
            peer,
            p.scope.digest(),
            BatchKind::Operation,
            &mut repair,
            &mut report,
        )
        .unwrap();
    assert_eq!(report.received, 0);
    assert_eq!(report.deferred, 1);
    let receipts = &stream.get_ref()[replies..];
    assert_eq!(receipts.len(), 33);
    assert_eq!(&receipts[..32], &id);
    assert_eq!(receipts[32], 0);
    let why = repair
        .batch_deferral
        .as_ref()
        .expect("storage refusal retained");
    assert_eq!(why.batch, "operations");
    assert_eq!(why.stage, "storage");
    assert!(!why.reason.is_empty());
}

#[test]
fn batch_refusal_metadata_is_bounded_without_splitting_utf8() {
    let why = BatchDeferral::new(
        "operations",
        "ingest",
        Error::repair(
            &"🧪".repeat(300),
            aterm_types::network_state::RepairCause::Quota,
        ),
    );
    assert_eq!(why.reason.chars().count(), 128);
    assert_eq!(why.reason.len(), 512);
}

#[test]
fn automatic_batches_recheck_local_policy_and_new_catalog_conflicts() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let config = lab.paths[0].home.join("config.toml");
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let peer = b.request().unwrap().binding().actor();
    let directory = crate::catalog::Directory::new(&lab.paths[0], a.trust());
    let sa = Session::open(&a).unwrap();
    let scope = directory
        .open_application(&sa, "accounts", "proposals", 1)
        .unwrap();
    drop(sa);
    let replica = Replica::commons(lab.paths[0].clone(), config.clone()).unwrap();
    let mut round = replica.round(peer).unwrap();
    round.active_table.set(Some(scope.digest()));
    let held = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    assert!(
        round
            .local(peer, |session| Ok(!session.has_control_writer()))
            .unwrap()
    );
    let tables = round
        .local(peer, |session| {
            directory.existing_subscriptions(session, &crate::catalog::policy::Policy::default())
        })
        .unwrap()
        .unwrap();
    assert!(tables.contains_key(&scope.digest()));
    round.tables = tables;
    assert!(
        round
            .inventory(peer, scope.digest(), &mut None, &mut None)
            .unwrap()
            .is_some()
    );
    drop(held);
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    assert!(
        round
            .local(peer, |_| -> Result<()> {
                panic!("disabled batch must not run")
            })
            .is_err()
    );
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
    assert!(round.local(peer, |_| Ok(())).is_ok());
    let sa = Session::open(&a).unwrap();
    let mut store = Store::open(
        &sa,
        &directory.path(directory.catalog_scope()).unwrap(),
        directory.catalog_scope(),
    )
    .unwrap();
    let incompatible =
        crate::catalog::Descriptor::application(a.trust(), "accounts", "proposals", 2).unwrap();
    store
        .commit_catalog(&sa, &incompatible.encode().unwrap())
        .unwrap();
    drop(store);
    drop(sa);
    assert!(
        round
            .local(peer, |_| -> Result<()> {
                panic!("conflicted table batch must not run")
            })
            .is_err()
    );
}

#[test]
fn automatic_application_refusal_preserves_control_for_retired_members() {
    use aterm_secrets::{
        client::{load_records, retain_records_for_commons, save_records, test_found},
        membership_log::{Body, Entrant, Log, Record, encode_records},
    };

    let p = Pair::new(false, 0);
    let founders: Vec<_> = (0..2)
        .map(|n| Entrant {
            request: p.lab.bootstrap(n).request().unwrap(),
            node_key: [u8::try_from(n).unwrap() + 1; 32],
            name: format!("recovery-{n}"),
            label: format!("m{}", n + 1),
        })
        .collect();
    let source = p.lab.bootstrap(0);
    let foreign = Record::found(&[7; 32], founders.clone(), 1_000).unwrap();
    assert_eq!(
        retain_records_for_commons(
            &p.lab.paths[1],
            source.trust().network,
            source.trust().authority,
            &[foreign.bytes().to_vec()],
        )
        .unwrap(),
        vec![false],
        "a foreign signer cannot establish this vault's recovery log"
    );
    // The vault's key signs the genesis: a joined Mac's repair takes no records under a genesis
    // another key signed (`retain_records_for_commons`, as above).
    let genesis = test_found(&p.lab.paths[0], founders, 1_000).unwrap();
    let request = source.request().unwrap();
    let retired = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let removal = source.device().with_seed(|seed| {
        Log::new(genesis.clone())
            .unwrap()
            .draft(
                &request.binding().actor(),
                1_001,
                Body::Remove {
                    member: retired,
                    cutoff: *genesis.id(),
                },
            )
            .sign(seed, *request.binding().incarnation())
            .unwrap()
    });
    save_records(&p.lab.paths[0], &encode_records([&genesis, &removal])).unwrap();
    save_records(&p.lab.paths[1], &encode_records([&genesis])).unwrap();
    assert!(
        !load_records(&p.lab.paths[1])
            .unwrap()
            .contains(removal.id())
    );
    p.retire_b();
    let admission = std::time::Instant::now() + Duration::from_secs(60);
    let a = Endpoint::from_bootstrap_until(&p.lab.bootstrap(0), admission)
        .unwrap()
        .unwrap()
        .with_commons_until(
            p.lab.paths[0].clone(),
            p.lab.paths[0].home.join("config"),
            admission,
        )
        .unwrap();
    let admission = std::time::Instant::now() + Duration::from_secs(60);
    let b = Endpoint::from_bootstrap_until(&p.lab.bootstrap(1), admission)
        .unwrap()
        .unwrap()
        .with_commons_until(
            p.lab.paths[1].clone(),
            p.lab.paths[1].home.join("config"),
            admission,
        )
        .unwrap();
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    assert!(ra.actor().is_some() && rb.actor().is_some());
    assert!(ra.application().is_none() && rb.application().is_none());
    assert!(ra.application_error().is_some() && rb.application_error().is_some());
    let retained = load_records(&p.lab.paths[1]).unwrap();
    assert_eq!(retained.get_bytes(removal.id()), Some(removal.bytes()));
    assert!(
        retained
            .log()
            .unwrap()
            .membership()
            .removal(&retired)
            .is_some()
    );
    assert_eq!(ra.membership().unwrap().sent, 1);
    assert_eq!(rb.membership().unwrap().received, 1);
    assert!(ra.membership_error().is_none() && rb.membership_error().is_none());
    assert!(Session::observe(&p.lab.bootstrap(1)).is_err());
}

#[test]
fn negotiated_repair_keeps_legacy_application_transfers_in_both_directions() {
    for (legacy, pre_f13) in [(0, true), (1, true), (0, false), (1, false)] {
        let p = Pair::new(false, 1);
        let mut endpoints = [p.endpoint(0, true), p.endpoint(1, true)];
        endpoints[legacy].legacy = pre_f13;
        endpoints[legacy].legacy_repair = !pre_f13;
        let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
        p.round(&endpoints[0], &endpoints[1], &mut ra, &mut rb);
        assert_eq!(p.count(0), 1);
        assert_eq!(p.count(1), 1);
        assert!(ra.application().is_some() && rb.application().is_some());
        assert!(ra.application_error().is_none() && rb.application_error().is_none());
        assert!(ra.membership_error().is_none() && rb.membership_error().is_none());
    }
}

#[test]
fn unreadable_membership_recovery_preserves_control_but_never_reports_application_success() {
    let p = Pair::new(false, 1);
    let endpoints = [p.endpoint(0, true), p.endpoint(1, true)];
    let custody = p.lab.bootstrap(1);
    let observed = Session::observe(&custody).unwrap();
    let table_before = std::fs::read(p.paths[1].join("table.astream")).unwrap();
    // Unreadable to custody, not merely damaged: a damaged file loads what still verifies
    // (`membership_log::Store::recover`), but one others may write is never read.
    aterm_secrets::fsx::write_atomic(&p.lab.paths[1].records_file(), b"invalid", 0o622).unwrap();
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&endpoints[0], &endpoints[1], &mut ra, &mut rb);
    for repair in [&ra, &rb] {
        assert!(repair.authenticated_actor().is_some());
        assert_eq!(repair.phase(), control::Phase::MembershipRecords);
        assert!(repair.membership().is_none());
        assert!(repair.membership_error().is_some());
        assert!(repair.application().is_none());
        assert!(repair.application_error().is_none());
    }
    assert!(Store::observe(&observed, &p.paths[1], p.scope).is_err());
    assert_eq!(
        std::fs::read(p.paths[1].join("table.astream")).unwrap(),
        table_before
    );
    assert!(Bootstrap::cached(&p.lab.paths[1]).is_err());
}

/// Membership records left for a later session hold up no table repair over `atdb/5`: each side's
/// store is full (a held total of two) and holds a record the other lacks, so each refuses the
/// other's, both stop the membership rounds after the same round with the records deferred, and
/// the session goes on to repair the table, which converges.
///
/// NEGATIVE CONTROL (in the test): the mutant that ends the session on deferred records
/// ([`crate::control::mutants::Mutant::DeferredEndsSession`], as the session did before this fix)
/// leaves the table unrepaired.
#[test]
fn deferred_membership_records_hold_up_no_table_repair() {
    use crate::control::mutants::{self, Mutant};
    use aterm_secrets::{
        client::{load_records, save_records, test_found},
        membership_log::{Body, Entrant, Log, RecordBudgets, encode_records},
    };
    const FULL: RecordBudgets = RecordBudgets {
        held: 2,
        ..RecordBudgets::PRODUCTION
    };
    for mutant in [false, true] {
        let p = Pair::new(false, 1);
        let founders: Vec<_> = (0..2)
            .map(|n| Entrant {
                request: p.lab.bootstrap(n).request().unwrap(),
                node_key: [u8::try_from(n).unwrap() + 1; 32],
                name: format!("full-{n}"),
                label: format!("m{}", n + 1),
            })
            .collect();
        let genesis = test_found(&p.lab.paths[0], founders, 1_000).unwrap();
        for n in 0..2 {
            let bootstrap = p.lab.bootstrap(n);
            let request = bootstrap.request().unwrap();
            let name = bootstrap.device().with_seed(|seed| {
                Log::new(genesis.clone())
                    .unwrap()
                    .draft(
                        &request.binding().actor(),
                        1_001,
                        Body::Name {
                            name: format!("only-{n}"),
                        },
                    )
                    .sign(seed, *request.binding().incarnation())
                    .unwrap()
            });
            save_records(&p.lab.paths[n], &encode_records([&genesis, &name])).unwrap();
        }
        let endpoints = FULL.within(|| [p.endpoint(0, true), p.endpoint(1, true)]);
        let session = |endpoint: &Endpoint, connection, side, repair: &mut PeerRepair| {
            FULL.within(|| {
                let run = || endpoint.exchange(connection, side, repair);
                if mutant {
                    mutants::with(Mutant::DeferredEndsSession, run)
                } else {
                    run()
                }
            })
        };
        let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::scope(|threads| {
            let server = threads.spawn(|| {
                let connection =
                    Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap();
                session(&endpoints[1], connection, Side::Responder, &mut rb)
            });
            let connection =
                Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap();
            session(&endpoints[0], connection, Side::Initiator, &mut ra).unwrap();
            server.join().unwrap().unwrap();
        });
        for repair in [&ra, &rb] {
            assert!(
                repair
                    .membership()
                    .is_some_and(|report| report.deferred > 0)
            );
            assert_eq!(repair.application().is_some(), !mutant, "mutant {mutant}");
        }
        assert_eq!(p.count(1), usize::from(!mutant), "mutant {mutant}");
        FULL.within(|| {
            for paths in &p.lab.paths {
                assert_eq!(load_records(paths).unwrap().len(), 2);
            }
        });
    }
}

/// A Mac whose records are not crossed completes table sync with a legacy peer, a build that
/// offers `atdb/3` alone, both holding a log of 4,071 records, near the legacy count: the hello's
/// witness, each inventory and the legacy witness decode stay within
/// [`aterm_secrets::membership_log::LEGACY_RECORDS`], and the table converges.
///
/// NEGATIVE CONTROL (in the test): the same pair with this Mac's records crossed refuses the legacy
/// peer before any membership frame, and the table stays unrepaired, so the session above ran the
/// legacy protocol.
#[test]
fn a_mac_not_crossed_completes_table_sync_with_a_legacy_peer_of_4071_records() {
    use aterm_secrets::{
        client::{load_records, save_records, test_found},
        membership_log::{Body, Entrant, Log, encode_records},
    };
    const HELD: usize = 4_071;
    for crossed in [false, true] {
        let p = Pair::new(false, 1);
        let founders: Vec<_> = (0..2)
            .map(|n| Entrant {
                request: p.lab.bootstrap(n).request().unwrap(),
                node_key: [u8::try_from(n).unwrap() + 1; 32],
                name: format!("legacy-{n}"),
                label: format!("m{}", n + 1),
            })
            .collect();
        let genesis = test_found(&p.lab.paths[0], founders, 1_000).unwrap();
        let bootstrap = p.lab.bootstrap(0);
        let request = bootstrap.request().unwrap();
        let log = bootstrap.device().with_seed(|seed| {
            let mut log = Log::new(genesis.clone()).unwrap();
            for n in 1..HELD as u64 {
                let record = log
                    .draft(
                        &request.binding().actor(),
                        1_000 + n,
                        Body::Name {
                            name: format!("host-{n}"),
                        },
                    )
                    .sign(seed, *request.binding().incarnation())
                    .unwrap();
                log.insert(record).unwrap();
            }
            log
        });
        assert_eq!(log.held_count(), HELD);
        for paths in &p.lab.paths {
            save_records(paths, &encode_records(log.records())).unwrap();
        }
        if crossed {
            aterm_secrets::fsx::write_atomic(
                &p.lab.paths[0].records_file().with_extension("crossed"),
                b"crossed\n",
                0o600,
            )
            .unwrap();
        }
        let mut endpoints = [p.endpoint(0, true), p.endpoint(1, true)];
        endpoints[1].legacy_credentials = true;
        let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (dialed, answered) = thread::scope(|threads| {
            let server = threads.spawn(|| {
                endpoints[1].exchange(
                    Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                    Side::Responder,
                    &mut rb,
                )
            });
            let dialed = endpoints[0].exchange(
                Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                &mut ra,
            );
            (dialed, server.join().unwrap())
        });
        if crossed {
            assert!(dialed.is_err() || answered.is_err());
            assert!(ra.application().is_none() && rb.application().is_none());
            assert!(ra.membership().is_none() && rb.membership().is_none());
            assert_eq!(p.count(1), 0);
            continue;
        }
        assert!(
            dialed.is_ok() && answered.is_ok(),
            "{dialed:?} {answered:?}"
        );
        for repair in [&ra, &rb] {
            assert!(repair.membership_error().is_none(), "{repair:?}");
            assert!(repair.application().is_some());
            assert!(repair.application_error().is_none());
        }
        assert_eq!(p.count(1), 1);
        for paths in &p.lab.paths {
            assert_eq!(load_records(paths).unwrap().len(), HELD);
        }
    }
}

#[test]
fn a_negotiated_control_only_peer_does_not_enter_application_repair() {
    let p = Pair::new(false, 1);
    let endpoints = [p.endpoint(0, false), p.endpoint(1, true)];
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&endpoints[0], &endpoints[1], &mut ra, &mut rb);
    for repair in [&ra, &rb] {
        assert_eq!(repair.phase(), control::Phase::ControlComplete);
        assert!(repair.membership().is_some());
        assert!(repair.membership_error().is_none());
        assert!(repair.application().is_none());
        assert!(repair.application_error().is_none());
    }
    assert_eq!(p.count(1), 0);
}

struct Pair {
    lab: Lab,
    scope: Scope,
    paths: [PathBuf; 2],
}
impl Pair {
    fn new(late: bool, records: usize) -> Self {
        let lab = if late {
            Lab::with_enrolled(&[Role::Writer, Role::Reader], 1)
        } else {
            Lab::new(&[Role::Writer, Role::Writer])
        };
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let paths = [
            lab._home.path().join("table-a"),
            lab._home.path().join("table-b"),
        ];
        let mut store = Store::create(&session, &paths[0]).unwrap();
        let scope = store.scope();
        for _ in 0..records {
            store.commit_catalog(&session, b"account-metadata").unwrap();
        }
        drop(store);
        drop(session);
        if late {
            lab.admit(&[Role::Writer, Role::Reader]);
        }
        let b = lab.bootstrap(1);
        let session = Session::open(&b).unwrap();
        Store::join(&session, &paths[1], scope).unwrap();
        drop(session);
        Self { lab, scope, paths }
    }
    fn endpoint(&self, n: usize, data: bool) -> Endpoint {
        let endpoint = Endpoint::from_bootstrap(&self.lab.bootstrap(n))
            .unwrap()
            .unwrap();
        if data {
            endpoint
                .with_tables(
                    self.lab.paths[n].clone(),
                    vec![(self.scope, self.paths[n].clone())],
                )
                .unwrap()
        } else {
            endpoint
        }
    }
    fn replica(&self, n: usize) -> Replica {
        Replica::new(
            self.lab.paths[n].clone(),
            vec![(self.scope, self.paths[n].clone())],
        )
        .unwrap()
    }
    fn count(&self, n: usize) -> usize {
        let bootstrap = self.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &self.paths[n], self.scope)
            .unwrap()
            .inventory(&session)
            .unwrap()
            .count()
    }
    fn round(&self, a: &Endpoint, b: &Endpoint, ra: &mut PeerRepair, rb: &mut PeerRepair) {
        exchange_round(a, b, ra, rb);
    }
    fn retire_b(&self) {
        let a = self.lab.bootstrap(0);
        let b = self.lab.bootstrap(1);
        let mut journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let admin = aterm_secrets::client::admin_open(&self.lab.paths[0]).unwrap();
        let node = admin
            .prepare_commons_retirement(
                &journal,
                vec![Grant::recorded(
                    "m1",
                    a.request().unwrap().binding().clone(),
                    Role::Writer,
                )],
                &[b.request().unwrap().binding().actor()],
            )
            .unwrap();
        journal.ingest(node.bytes()).unwrap();
        drop(journal);
        Journal::open_existing(b.journal_dir(), b.trust())
            .unwrap()
            .ingest(node.bytes())
            .unwrap();
    }
}

fn exchange_round(a: &Endpoint, b: &Endpoint, ra: &mut PeerRepair, rb: &mut PeerRepair) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::scope(|threads| {
        let server = threads.spawn(|| {
            b.exchange(
                Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                Side::Responder,
                rb,
            )
        });
        a.exchange(
            Connection::connect(address, LinkScope::Loopback, Duration::from_secs(2)).unwrap(),
            Side::Initiator,
            ra,
        )
        .unwrap();
        server.join().unwrap().unwrap();
    });
}

#[test]
fn tls_inventory_evidence_precedes_positive_repair_and_distinguishes_equal_counts() {
    let p = Pair::new(false, 1);
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    Store::open(&session, &p.paths[1], p.scope)
        .unwrap()
        .commit_catalog(&session, b"different offline operation")
        .unwrap();
    drop(session);
    let inventory = |n| {
        let bootstrap = p.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        let store = Store::open(&session, &p.paths[n], p.scope).unwrap();
        InventorySummary::of(&store.inventory(&session).unwrap().copied().collect())
    };
    let before = [inventory(0), inventory(1)];
    assert_eq!(before[0].count, before[1].count);
    assert_ne!(before[0].fingerprint, before[1].fingerprint);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    for (n, repair) in [&ra, &rb].into_iter().enumerate() {
        let report = repair.application().unwrap();
        assert_eq!(report.local_known, Some(before[n]));
        assert_eq!(report.local_offered, Some(before[n]));
        assert_eq!(report.peer_known, Some(before[1 - n]));
        assert!(
            report.matching_inventory.is_none(),
            "equal counts with different retained IDs are not a match"
        );
        assert_eq!((report.sent, report.received), (1, 1));
        assert!(repair.application_error().is_none());
        assert_eq!(p.count(n), 2);
    }
    let after = inventory(0);
    assert_eq!(after, inventory(1));
    p.round(&a, &b, &mut ra, &mut rb);
    for (n, repair) in [&ra, &rb].into_iter().enumerate() {
        let report = repair.application().unwrap();
        assert_eq!(report.local_known, Some(after));
        assert_eq!(report.local_offered, Some(after));
        assert_eq!(report.peer_known, Some(after));
        assert_eq!((report.sent, report.received), (0, 0));
        let matched = report
            .matching_inventory
            .expect("the next completed round observes matching retained IDs");
        assert_eq!(matched.scope(), p.scope);
        assert_eq!(matched.inventory(), after);
        let bootstrap = p.lab.bootstrap(n);
        let session = Session::observe(&bootstrap).unwrap();
        assert_eq!(matched.local_actor(), session.actor());
        assert_eq!(
            matched.control_observation(),
            session.observation().unwrap()
        );
        assert_eq!(Some(matched.peer_actor()), repair.authenticated_actor());
        assert_ne!(matched.observed_at_ms(), 0);
    }
}

#[test]
fn the_final_matching_inventory_read_rejects_real_inventory_and_control_changes() {
    let p = Pair::new(false, 0);
    let replica = p.replica(0);
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let id = p.scope.digest();
    let make_report = |round: &Round<'_>| {
        let before = round
            .inventory(peer, id, &mut None, &mut None)
            .unwrap()
            .unwrap()
            .observation;
        round.inventory_observation.set(Some(before));
        round.active_table.set(Some(id));
        Report {
            table: Some(id),
            local_known: Some(before.inventory),
            peer_known: Some(before.inventory),
            local_offered: Some(before.inventory),
            ..Report::default()
        }
    };
    let round = replica.round(peer).unwrap();
    let report = make_report(&round);
    assert!(round.matching_inventory(peer, id, &report).is_some());

    let source = p.lab.bootstrap(0);
    let session = Session::open(&source).unwrap();
    Store::open(&session, &p.paths[0], p.scope)
        .unwrap()
        .commit_catalog(&session, b"a concurrent operation after inventory exchange")
        .unwrap();
    drop(session);
    drop(source);
    assert!(
        round.matching_inventory(peer, id, &report).is_none(),
        "a final inventory change withdraws the observation"
    );

    let round = replica.round(peer).unwrap();
    let report = make_report(&round);
    assert!(round.matching_inventory(peer, id, &report).is_some());
    p.retire_b();
    assert!(
        round.matching_inventory(peer, id, &report).is_none(),
        "changed control and retired peer refuse the final admitted read"
    );
}

#[test]
fn tls_fetches_historical_keys_then_repairs_pages_and_restarts() {
    let p = Pair::new(true, 35);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    let (mut keys_sent, mut keys_received) = (0, 0);
    for _ in 0..3 {
        p.round(&a, &b, &mut ra, &mut rb);
        keys_sent += ra.application().unwrap().keys_sent;
        keys_received += rb.application().unwrap().keys_received;
    }
    assert!(
        keys_sent > 0,
        "the historical handoff must be observed as an inspector effect"
    );
    assert_eq!(keys_sent, keys_received);
    assert_eq!(p.count(1), 35);
    assert_eq!(ra.application().unwrap().sent, 3);
    let history = p.lab.paths[1].dir.join("commons/history");
    let files: Vec<_> = std::fs::read_dir(history)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    std::fs::remove_file(&files[0]).unwrap();
    // Table::open now refuses its old records; control-derived requests still
    // obtain the missing key before the next table inventory tries to open it.
    p.round(&a, &b, &mut ra, &mut rb);
    assert_eq!(ra.application().unwrap().keys_sent, 1);
    assert_eq!(rb.application().unwrap().keys_received, 1);
    assert_eq!(p.count(1), 35);
    p.round(&a, &b, &mut ra, &mut rb);
    assert_eq!(ra.application().unwrap().sent, 0);
}

fn old_peers_still_complete_control_repair_in_both_directions() -> (i64, i64, i64) {
    let p = Pair::new(false, 1);
    for (data_a, data_b) in [(true, false), (false, true)] {
        let (a, b) = (p.endpoint(0, data_a), p.endpoint(1, data_b));
        let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
        p.round(&a, &b, &mut ra, &mut rb);
        assert!(ra.actor().is_some() && rb.actor().is_some());
        assert!(ra.application().is_none() && rb.application().is_none());
        assert_eq!(p.count(1), 0);
    }
    (p.count(1) as i64, 0, 1)
}

fn revocation_after_endpoint_creation_withholds_application_data() -> (i64, i64, i64) {
    let p = Pair::new(false, 1);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let before = std::fs::read(p.paths[1].join("table.astream")).unwrap();
    p.retire_b();
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    assert!(ra.application().is_none() && rb.application().is_none());
    assert_eq!(
        std::fs::read(p.paths[1].join("table.astream")).unwrap(),
        before
    );
    (
        0,
        0,
        i64::from(ra.actor().is_some() && rb.actor().is_some()),
    )
}

#[test]
fn inbound_rounds_do_not_rewind_the_outbound_table_cursor() {
    table_scan_case(false, false);
}

#[test]
fn new_peers_start_on_different_tables_and_resume_their_own_scan() {
    let lab = Lab::new(&[Role::Writer; 4]);
    let mut tables = Vec::new();
    for table in 0..3 {
        let paths: [PathBuf; 4] =
            std::array::from_fn(|node| lab._home.path().join(format!("fresh-{table}-{node}")));
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = Store::create(&session, &paths[0]).unwrap().scope();
        drop(session);
        for (node, path) in paths.iter().enumerate().skip(1) {
            let bootstrap = lab.bootstrap(node);
            let session = Session::open(&bootstrap).unwrap();
            Store::join(&session, path, scope).unwrap();
        }
        tables.push((scope, paths));
    }
    tables.sort_by_key(|(scope, _)| scope.digest());
    // Each new peer supplies a different table. The first sweep must not
    // keep asking all three peers about the receiver's first table.
    for (table, (scope, paths)) in tables.iter().enumerate() {
        let node = table + 1;
        let bootstrap = lab.bootstrap(node);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &paths[node], *scope)
            .unwrap()
            .commit_catalog(&session, b"first-contact backfill")
            .unwrap();
    }
    let endpoints: Vec<_> = (0..4)
        .map(|node| {
            Endpoint::from_bootstrap(&lab.bootstrap(node))
                .unwrap()
                .unwrap()
                .with_tables(
                    lab.paths[node].clone(),
                    tables
                        .iter()
                        .map(|(scope, paths)| (*scope, paths[node].clone()))
                        .collect(),
                )
                .unwrap()
        })
        .collect();
    let mut outbound: [PeerRepair; 3] = std::array::from_fn(|_| PeerRepair::default());
    let mut inbound: [PeerRepair; 3] = std::array::from_fn(|_| PeerRepair::default());
    for peer in 0..3 {
        exchange_round(
            &endpoints[0],
            &endpoints[peer + 1],
            &mut outbound[peer],
            &mut inbound[peer],
        );
    }
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    for (scope, paths) in &tables {
        let store = Store::observe(&session, &paths[0], *scope).unwrap();
        assert_eq!(store.inventory(&session).unwrap().count(), 1);
    }
    drop(session);
    // Once a peer has a cursor, its next ordinary scan continues from it,
    // independently of the starting positions given to the other peers.
    for peer in 0..3 {
        exchange_round(
            &endpoints[0],
            &endpoints[peer + 1],
            &mut outbound[peer],
            &mut inbound[peer],
        );
        assert_eq!(
            outbound[peer].application().unwrap().table,
            Some(tables[(peer + 1) % 3].0.digest())
        );
    }
}

#[test]
fn full_batches_continue_without_starving_another_table() {
    for initiator in 0..2 {
        let pair = Pair::new(false, 0);
        let paths = [
            pair.lab._home.path().join("second-a"),
            pair.lab._home.path().join("second-b"),
        ];
        let bootstrap = pair.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = Store::create(&session, &paths[0]).unwrap().scope();
        drop(session);
        let bootstrap = pair.lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        Store::join(&session, &paths[1], scope).unwrap();
        drop(session);
        let mut tables = [(pair.scope, pair.paths.clone()), (scope, paths)];
        tables.sort_by_key(|(scope, _)| scope.digest());
        let count = BATCH * TABLE_BURST + 1;
        let bootstrap = pair.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        for (index, (scope, paths)) in tables.iter().enumerate() {
            let mut store = Store::open(&session, &paths[0], *scope).unwrap();
            for _ in 0..if index == 0 { count } else { 1 } {
                store.commit_catalog(&session, b"backfill fixture").unwrap();
            }
        }
        drop(session);
        let endpoints: Vec<_> = (0..2)
            .map(|node| {
                Endpoint::from_bootstrap(&pair.lab.bootstrap(node))
                    .unwrap()
                    .unwrap()
                    .with_tables(
                        pair.lab.paths[node].clone(),
                        tables
                            .iter()
                            .map(|(scope, paths)| (*scope, paths[node].clone()))
                            .collect(),
                    )
                    .unwrap()
            })
            .collect();
        let mut sent = PeerRepair::default();
        let mut received = PeerRepair::default();
        for turn in 0..TABLE_BURST + 2 {
            pair.round(
                &endpoints[initiator],
                &endpoints[1 - initiator],
                &mut sent,
                &mut received,
            );
            let report = sent.application().unwrap();
            let selected = usize::from(turn == TABLE_BURST);
            assert_eq!(report.table, Some(tables[selected].0.digest()));
            let transferred = if initiator == 0 {
                report.sent
            } else {
                report.received
            };
            assert_eq!(transferred, if turn < TABLE_BURST { BATCH } else { 1 });
        }
        let bootstrap = pair.lab.bootstrap(1);
        let session = Session::observe(&bootstrap).unwrap();
        for (index, (scope, paths)) in tables.iter().enumerate() {
            let store = Store::observe(&session, &paths[1], *scope).unwrap();
            assert_eq!(
                store.inventory(&session).unwrap().count(),
                if index == 0 { count } else { 1 }
            );
        }
    }
}

#[test]
fn incoming_backlog_guides_a_different_peer_without_starving_regular_tables() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let mut tables = Vec::new();
    for index in 0..2 {
        let paths: [PathBuf; 3] =
            std::array::from_fn(|node| lab._home.path().join(format!("backlog-{index}-{node}")));
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = Store::create(&session, &paths[0]).unwrap().scope();
        drop(session);
        for (node, path) in paths.iter().enumerate().skip(1) {
            let bootstrap = lab.bootstrap(node);
            let session = Session::open(&bootstrap).unwrap();
            Store::join(&session, path, scope).unwrap();
        }
        tables.push((scope, paths));
    }
    tables.sort_by_key(|(scope, _)| scope.digest());
    let count = BATCH * (TABLE_BURST + 1) + 1;
    for (index, (scope, paths)) in tables.iter().enumerate() {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &paths[0], *scope).unwrap();
        let mut wires = Vec::new();
        for _ in 0..if index == 0 { 1 } else { count } {
            let id = store
                .commit_catalog(&session, b"shared backlog")
                .unwrap()
                .id;
            wires.push(store.wire(&session, &id).unwrap().unwrap().to_vec());
        }
        drop(store);
        drop(session);
        let bootstrap = lab.bootstrap(2);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &paths[2], *scope).unwrap();
        for wire in wires {
            store.ingest(&session, &wire).unwrap();
        }
    }
    let cache = crate::control::PeerCache::default();
    let endpoint = |node| {
        let endpoint = Endpoint::from_bootstrap(&lab.bootstrap(node))
            .unwrap()
            .unwrap()
            .with_tables(
                lab.paths[node].clone(),
                tables
                    .iter()
                    .map(|(scope, paths)| (*scope, paths[node].clone()))
                    .collect(),
            )
            .unwrap();
        if node == 1 {
            endpoint.with_repair_cache(&cache)
        } else {
            endpoint
        }
    };
    let mut endpoints: Vec<_> = (0..3).map(&endpoint).collect();
    let (mut source, mut inbound) = (PeerRepair::default(), PeerRepair::default());
    // An incoming peer first supplies the small table, then one full batch.
    for _ in 0..2 {
        exchange_round(&endpoints[0], &endpoints[1], &mut source, &mut inbound);
    }
    assert_eq!(inbound.application().unwrap().received, BATCH);
    // An ordinary owner rebuild keeps work hints for this same trust and actor.
    endpoints[1] = endpoint(1);

    // This is a new outgoing peer: it has no per-peer table cursor to resume.
    let (mut outbound, mut other) = (PeerRepair::default(), PeerRepair::default());
    for _ in 0..TABLE_BURST {
        exchange_round(&endpoints[1], &endpoints[2], &mut outbound, &mut other);
        let report = outbound.application().unwrap();
        assert_eq!(report.table, Some(tables[1].0.digest()));
        assert_eq!(report.received, BATCH);
    }
    exchange_round(&endpoints[1], &endpoints[2], &mut outbound, &mut other);
    assert_eq!(
        outbound.application().unwrap().table,
        Some(tables[0].0.digest())
    );
    exchange_round(&endpoints[1], &endpoints[2], &mut outbound, &mut other);
    assert_eq!(outbound.application().unwrap().received, 1);
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let store = Store::observe(&session, &tables[1].1[1], tables[1].0).unwrap();
    assert_eq!(store.inventory(&session).unwrap().count(), count);
}

#[test]
fn duplicate_receipts_do_not_clear_other_missing_ids() {
    let table = [9; 32];
    let tables = BTreeSet::from([table]);
    let known = BTreeSet::from([[1; 32], [2; 32]]);
    let remote = BTreeSet::from([[1; 32], [2; 32], [3; 32], [4; 32]]);
    let mut backfill = Backfill::default();
    backfill.observe(table, &known, &remote);
    backfill.retained(table, [3; 32]);
    backfill.retained(table, [3; 32]);
    // A newer local count and a less-informed peer still supply no record 4.
    let grown = BTreeSet::from([[1; 32], [2; 32], [3; 32], [8; 32]]);
    backfill.observe(table, &grown, &known);
    assert_eq!(backfill.next(&tables, &tables), Some(table));
    backfill.retained(table, [4; 32]);
    assert_eq!(backfill.next(&tables, &tables), None);
}

#[test]
fn missing_id_hints_keep_inventory_and_table_bounds() {
    let table = [9; 32];
    let inventory = |start| {
        (start..start + MAX_RECORDS)
            .map(|index| {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
                id
            })
            .collect::<BTreeSet<_>>()
    };
    let first = inventory(0);
    let second = inventory(MAX_RECORDS);
    let mut backfill = Backfill::default();
    backfill.observe(table, &BTreeSet::new(), &first);
    backfill.observe(table, &BTreeSet::new(), &second);
    assert_eq!(backfill.wanted[&table], first);
    // Learning the first set locally frees its slots for the next inventory.
    backfill.observe(table, &first, &second);
    assert_eq!(backfill.wanted[&table], second);
    for index in 0..=MAX_TABLES {
        let mut other = [0; 32];
        other[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
        backfill.observe(other, &BTreeSet::new(), &BTreeSet::from([[5; 32]]));
    }
    assert_eq!(backfill.wanted.len(), MAX_TABLES);
}

#[test]
fn concurrent_local_writes_do_not_erase_a_remote_backlog() {
    let lab = Lab::new(&[Role::Writer; 3]);
    let mut tables = Vec::new();
    for index in 0..2 {
        let paths: [PathBuf; 3] =
            std::array::from_fn(|node| lab._home.path().join(format!("growing-{index}-{node}")));
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = Store::create(&session, &paths[0]).unwrap().scope();
        drop(session);
        for (node, path) in paths.iter().enumerate().skip(1) {
            let bootstrap = lab.bootstrap(node);
            let session = Session::open(&bootstrap).unwrap();
            Store::join(&session, path, scope).unwrap();
        }
        tables.push((scope, paths));
    }
    tables.sort_by_key(|(scope, _)| scope.digest());
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    for (index, (scope, paths)) in tables.iter().enumerate() {
        let mut store = Store::open(&session, &paths[0], *scope).unwrap();
        for _ in 0..if index == 0 { 1 } else { 2 * BATCH + 1 } {
            store.commit_catalog(&session, b"source backlog").unwrap();
        }
    }
    drop(session);
    let cache = crate::control::PeerCache::default();
    let endpoints: Vec<_> = (0..3)
        .map(|node| {
            let endpoint = Endpoint::from_bootstrap(&lab.bootstrap(node))
                .unwrap()
                .unwrap()
                .with_tables(
                    lab.paths[node].clone(),
                    tables
                        .iter()
                        .map(|(scope, paths)| (*scope, paths[node].clone()))
                        .collect(),
                )
                .unwrap();
            if node == 1 {
                endpoint.with_repair_cache(&cache)
            } else {
                endpoint
            }
        })
        .collect();
    let (mut source, mut receiver) = (PeerRepair::default(), PeerRepair::default());
    for _ in 0..2 {
        exchange_round(&endpoints[0], &endpoints[1], &mut source, &mut receiver);
    }
    assert_eq!(receiver.application().unwrap().received, BATCH);

    // These new records bring the receiver's count up to the source's old
    // count, but none fills a hole left by the source's first batch.
    let bootstrap = lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &tables[1].1[1], tables[1].0).unwrap();
    for _ in 0..BATCH + 1 {
        store
            .commit_catalog(&session, b"unrelated local write")
            .unwrap();
    }
    drop(store);
    drop(session);

    // A less-informed peer offers no missing source records. Its round
    // must not erase the backlog before the receiver asks the source again.
    let (mut outbound, mut less_informed) = (PeerRepair::default(), PeerRepair::default());
    exchange_round(
        &endpoints[1],
        &endpoints[2],
        &mut outbound,
        &mut less_informed,
    );
    assert_eq!(
        outbound.application().unwrap().table,
        Some(tables[1].0.digest())
    );
    exchange_round(&endpoints[1], &endpoints[0], &mut receiver, &mut source);
    let recovered = receiver.application().unwrap();
    assert_eq!(
        recovered.table,
        Some(tables[1].0.digest()),
        "matching record counts must not discard missing record identities"
    );
    assert_eq!(recovered.received, BATCH);
    for _ in 0..TABLE_BURST + 1 {
        exchange_round(&endpoints[1], &endpoints[0], &mut receiver, &mut source);
    }
    let inventories: Vec<BTreeSet<_>> = (0..2)
        .map(|node| {
            let bootstrap = lab.bootstrap(node);
            let session = Session::observe(&bootstrap).unwrap();
            Store::observe(&session, &tables[1].1[node], tables[1].0)
                .unwrap()
                .inventory(&session)
                .unwrap()
                .copied()
                .collect()
        })
        .collect();
    assert_eq!(inventories[0], inventories[1]);
    assert_eq!(inventories[1].len(), 3 * BATCH + 2);
}

#[test]
fn empty_regular_scans_do_not_restore_a_spent_hint_allowance() {
    let table = [1; 32];
    let other = [2; 32];
    let tables = BTreeSet::from([table, other]);
    let mut backfill = Backfill::default();
    backfill.observe(table, &BTreeSet::new(), &BTreeSet::from([[3; 32]]));
    for _ in 0..TABLE_BURST {
        assert_eq!(backfill.next(&tables, &tables), Some(table));
    }
    assert_eq!(backfill.next(&tables, &tables), None);
    assert_eq!(backfill.regular(&BTreeSet::new(), None), None);
    assert_eq!(backfill.next(&tables, &tables), None);
    assert_eq!(backfill.regular(&tables, Some(table)), Some(other));
    assert_eq!(backfill.next(&tables, &tables), Some(table));
}

#[test]
fn stale_backlog_yields_across_first_contact_peers() {
    let nodes = TABLE_BURST + 4;
    let roles = vec![Role::Writer; nodes];
    let lab = Lab::new(&roles);
    let mut tables = Vec::new();
    for index in 0..2 {
        let paths: Vec<_> = (0..nodes)
            .map(|node| {
                lab._home
                    .path()
                    .join(format!("first-contact-{index}-{node}"))
            })
            .collect();
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = Store::create(&session, &paths[0]).unwrap().scope();
        drop(session);
        for (node, path) in paths.iter().enumerate().skip(1) {
            let bootstrap = lab.bootstrap(node);
            let session = Session::open(&bootstrap).unwrap();
            Store::join(&session, path, scope).unwrap();
        }
        tables.push((scope, paths));
    }
    tables.sort_by_key(|(scope, _)| scope.digest());
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut source = Store::open(&session, &tables[0].1[0], tables[0].0).unwrap();
    for _ in 0..BATCH + 1 {
        source
            .commit_catalog(&session, b"temporarily absent source")
            .unwrap();
    }
    drop(source);
    drop(session);
    for node in 3..nodes {
        let bootstrap = lab.bootstrap(node);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &tables[1].1[node], tables[1].0)
            .unwrap()
            .commit_catalog(&session, b"available on a fresh peer")
            .unwrap();
    }
    let cache = crate::control::PeerCache::default();
    let endpoints: Vec<_> = (0..nodes)
        .map(|node| {
            let endpoint = Endpoint::from_bootstrap(&lab.bootstrap(node))
                .unwrap()
                .unwrap()
                .with_tables(
                    lab.paths[node].clone(),
                    tables
                        .iter()
                        .map(|(scope, paths)| (*scope, paths[node].clone()))
                        .collect(),
                )
                .unwrap();
            if node == 1 {
                endpoint.with_repair_cache(&cache)
            } else {
                endpoint
            }
        })
        .collect();
    // The receiver's ordinary first-contact scan has visited table zero.
    exchange_round(
        &endpoints[1],
        &endpoints[2],
        &mut PeerRepair::default(),
        &mut PeerRepair::default(),
    );
    let (mut source_state, mut received) = (PeerRepair::default(), PeerRepair::default());
    exchange_round(
        &endpoints[0],
        &endpoints[1],
        &mut source_state,
        &mut received,
    );
    assert_eq!(received.application().unwrap().received, BATCH);

    // Each available peer gets one contact. None has the missing source
    // record, but all have useful data in the other table.
    for node in 3..nodes {
        exchange_round(
            &endpoints[1],
            &endpoints[node],
            &mut PeerRepair::default(),
            &mut PeerRepair::default(),
        );
    }
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let available = Store::observe(&session, &tables[1].1[1], tables[1].0)
        .unwrap()
        .inventory(&session)
        .unwrap()
        .count();
    assert!(available > 0, "a stale hint consumed every first contact");
    drop(session);

    // Yielding the scan preserves the original missing record for when its
    // source participates again.
    exchange_round(
        &endpoints[0],
        &endpoints[1],
        &mut source_state,
        &mut received,
    );
    assert_eq!(received.application().unwrap().received, 1);
}

#[test]
fn rebuilt_endpoints_keep_the_scan_without_preserving_revoked_access() {
    table_scan_case(true, false);
    table_scan_case(true, true);
}

fn table_scan_case(rebuild: bool, retain_hints: bool) {
    let p = Pair::new(false, 0);
    let mut tables = vec![(p.scope, p.paths.clone())];
    for index in 1..3 {
        let paths = [
            p.lab._home.path().join(format!("cursor-{index}-a")),
            p.lab._home.path().join(format!("cursor-{index}-b")),
        ];
        let bootstrap = p.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = Store::create(&session, &paths[0]).unwrap().scope();
        drop(session);
        let bootstrap = p.lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        Store::join(&session, &paths[1], scope).unwrap();
        drop(session);
        tables.push((scope, paths));
    }
    tables.sort_by_key(|(scope, _)| scope.digest());
    let bootstrap = p.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let last = &tables[2];
    let expected = Store::open(&session, &last.1[0], last.0)
        .unwrap()
        .commit_catalog(&session, b"last table must not starve")
        .unwrap()
        .id;
    drop(session);
    let endpoint = |node| {
        Endpoint::from_bootstrap(&p.lab.bootstrap(node))
            .unwrap()
            .unwrap()
            .with_tables(
                p.lab.paths[node].clone(),
                tables
                    .iter()
                    .map(|(scope, paths)| (*scope, paths[node].clone()))
                    .collect(),
            )
            .unwrap()
    };
    let cache = crate::control::PeerCache::default();
    let sender = || {
        let endpoint = endpoint(0);
        if retain_hints {
            endpoint.with_repair_cache(&cache)
        } else {
            endpoint
        }
    };
    let mut stable = sender();
    let mut outbound = PeerRepair::default();
    for (scope, _) in &tables {
        if rebuild {
            stable = sender();
            outbound = PeerRepair::default();
        }
        p.round(
            &stable,
            &endpoint(1),
            &mut outbound,
            &mut PeerRepair::default(),
        );
        let selected = if rebuild && !retain_hints {
            tables[0].0
        } else {
            *scope
        };
        assert_eq!(
            outbound.application().unwrap().table,
            Some(selected.digest())
        );
        // A restarted peer begins its own scan at the first table. Its inbound
        // selection must not rewind the stable endpoint's outgoing scan.
        let mut incoming = PeerRepair::default();
        p.round(
            &endpoint(1),
            &stable,
            &mut PeerRepair::default(),
            &mut incoming,
        );
        assert_eq!(
            incoming.application().unwrap().table,
            Some(tables[0].0.digest())
        );
    }
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let retained = Store::open(&session, &last.1[1], last.0).unwrap();
    assert_eq!(
        retained
            .inventory(&session)
            .unwrap()
            .copied()
            .collect::<BTreeSet<_>>(),
        if rebuild && !retain_hints {
            BTreeSet::new()
        } else {
            BTreeSet::from([expected])
        }
    );
    drop(retained);
    drop(session);
    if retain_hints {
        let old_peer = endpoint(1);
        let before = std::fs::read(last.1[1].join("table.astream")).unwrap();
        p.retire_b();
        let bootstrap = p.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &last.1[0], last.0)
            .unwrap()
            .commit_catalog(&session, b"after peer removal")
            .unwrap();
        drop(session);
        let (mut sent, mut received) = (PeerRepair::default(), PeerRepair::default());
        p.round(&sender(), &old_peer, &mut sent, &mut received);
        assert!(sent.application().is_none() && received.application().is_none());
        assert_eq!(
            std::fs::read(last.1[1].join("table.astream")).unwrap(),
            before
        );
    }
}

#[test]
fn unavailable_inventories_preserve_the_local_refusal_and_clear_after_repair() {
    for broken in 0..2 {
        let p = Pair::new(false, 1);
        let endpoints = [p.endpoint(0, true), p.endpoint(1, true)];
        let checkpoint = p.paths[broken].join("table.checkpoint");
        let saved = p.paths[broken].join("saved-checkpoint");
        let original = std::fs::read(p.paths[broken].join("table.astream")).unwrap();
        std::fs::rename(&checkpoint, &saved).unwrap();
        let bootstrap = p.lab.bootstrap(broken);
        let session = Session::observe(&bootstrap).unwrap();
        let error = Store::open(&session, &p.paths[broken], p.scope)
            .err()
            .unwrap()
            .to_string();
        drop(session);
        let mut repairs = [PeerRepair::default(), PeerRepair::default()];
        let [ra, rb] = &mut repairs;
        p.round(&endpoints[0], &endpoints[1], ra, rb);
        for (node, repair) in repairs.iter().enumerate() {
            assert!(repair.application_error().is_none());
            let report = repair.application().unwrap();
            assert_eq!(report.table, Some(p.scope.digest()));
            assert_eq!((report.sent, report.received, report.deferred), (0, 0, 1));
            assert!(report.local_known.is_none() && report.peer_known.is_none());
            let why = repair.application_deferral().unwrap();
            assert_eq!(why.inventory, "conflicts");
            assert_eq!(
                why.local_error.as_deref(),
                (node == broken).then_some(error.as_str())
            );
            assert_eq!(why.peer_unavailable, node != broken);
            assert!(!why.retaining_primary_conflicts);
        }
        assert!(!checkpoint.exists());
        assert_eq!(
            std::fs::read(p.paths[broken].join("table.astream")).unwrap(),
            original
        );
        std::fs::rename(&saved, &checkpoint).unwrap();
        let [ra, rb] = &mut repairs;
        p.round(&endpoints[0], &endpoints[1], ra, rb);
        for repair in &repairs {
            assert!(repair.application_deferral().is_none());
            assert!(repair.application_error().is_none());
            assert_eq!(repair.application().unwrap().deferred, 0);
        }
        assert_eq!((p.count(0), p.count(1)), (1, 1));
    }
}

fn damaged_table_is_unavailable_and_never_reset_or_acknowledged() -> (i64, i64, i64) {
    let p = Pair::new(false, 1);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    std::fs::remove_file(p.paths[1].join("table.checkpoint")).unwrap();
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    assert_eq!(ra.application().unwrap().sent, 0);
    assert_eq!(ra.application().unwrap().deferred, 1);
    assert!(!p.paths[1].join("table.checkpoint").exists());
    (
        ra.application().unwrap().sent as i64,
        0,
        i64::from(ra.actor().is_some() && rb.actor().is_some()),
    )
}

/// The two cases where Mac 0 initiates: a sender that initiates and a sender that responds, each
/// Mac sending once, so each Mac's damaged copy is met. Both Macs are writers on one table, and
/// swapping them only relabels the exchange. GAP: Mac 1 initiates in neither, so a defect only at
/// (Mac 0 sends, Mac 1 initiates) or (Mac 1 sends, Mac 1 initiates) is not seen.
fn a_damaged_neighbor_does_not_starve_healthy_table_repair() -> [i64; 4] {
    let mut observed = [1, 1, 1, 0];
    for case in aterm_spec::fs_lane::each(&[0, 2], |&case| damaged_neighbor_case(case)) {
        for (all, this) in observed.iter_mut().zip(case).take(3) {
            *all &= this;
        }
        observed[3] |= case[3];
    }
    observed
}

/// One (sender, initiator) case of [`a_damaged_neighbor_does_not_starve_healthy_table_repair`],
/// on its own Lab: whether the healthy table was stored, acknowledged and authenticated, and
/// whether the damaged one was ever sent or received.
fn damaged_neighbor_case(case: usize) -> [i64; 4] {
    let records = BATCH * 2 + 3;
    let mut observed = [1, 1, 1, 0];
    let sender = case / 2;
    let receiver = 1 - sender;
    let initiator = case % 2;
    let p = Pair::new(false, 0);
    let other_paths = [
        p.lab._home.path().join("other-a"),
        p.lab._home.path().join("other-b"),
    ];
    let bootstrap = p.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let other_scope = Store::create(&session, &other_paths[0]).unwrap().scope();
    drop(session);
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    Store::join(&session, &other_paths[1], other_scope).unwrap();
    drop(session);
    let tables = [(p.scope, p.paths.clone()), (other_scope, other_paths)];
    let damaged = usize::from(tables[0].0.digest() > tables[1].0.digest());
    let healthy = 1 - damaged;
    let bootstrap = p.lab.bootstrap(sender);
    let session = Session::open(&bootstrap).unwrap();
    for (index, (scope, paths)) in tables.iter().enumerate() {
        let mut store = Store::open(&session, &paths[sender], *scope).unwrap();
        for _ in 0..if index == healthy { records } else { 1 } {
            store.commit_catalog(&session, b"offline edit").unwrap();
        }
    }
    let expected: BTreeSet<_> =
        Store::open(&session, &tables[healthy].1[sender], tables[healthy].0)
            .unwrap()
            .inventory(&session)
            .unwrap()
            .copied()
            .collect();
    assert_eq!(expected.len(), records);
    drop(session);
    let broken = &tables[damaged].1[receiver];
    let original = std::fs::read(broken.join("table.astream")).unwrap();
    std::fs::remove_file(broken.join("table.checkpoint")).unwrap();
    let endpoint = |node| {
        Endpoint::from_bootstrap(&p.lab.bootstrap(node))
            .unwrap()
            .unwrap()
            .with_tables(
                p.lab.paths[node].clone(),
                tables
                    .iter()
                    .map(|(scope, paths)| (*scope, paths[node].clone()))
                    .collect(),
            )
            .unwrap()
    };
    let mut a = endpoint(initiator);
    let mut b = endpoint(1 - initiator);
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    let mut acknowledged = 0;
    let mut refused = 0;
    let mut stored = 0;
    for turn in 0..6 {
        if turn == 1 {
            // Reconstruct both endpoints after a failed table, retaining only
            // the per-peer repair hints owned by the calling transport.
            a = endpoint(initiator);
            b = endpoint(1 - initiator);
        }
        p.round(&a, &b, &mut ra, &mut rb);
        let (offered, received) = if sender == initiator {
            (ra.application().unwrap(), rb.application().unwrap())
        } else {
            (rb.application().unwrap(), ra.application().unwrap())
        };
        let selected = tables
            .iter()
            .position(|(scope, _)| offered.table == Some(scope.digest()))
            .expect("one of the provisioned tables was selected");
        assert_eq!(received.table, offered.table);
        let authenticated = ra.actor().is_some() && rb.actor().is_some();
        assert!(authenticated);
        observed[2] &= i64::from(authenticated);
        if selected == damaged {
            observed[3] |= i64::from(offered.sent > 0 || received.received > 0);
            assert_eq!((offered.sent, received.received), (0, 0));
            assert_eq!((offered.deferred, received.deferred), (1, 1));
            assert!(!offered.caught_up, "a deferred table caught nobody up");
            refused += 1;
        } else {
            let expected = BATCH.min(records - acknowledged);
            assert_eq!((offered.sent, received.received), (expected, expected));
            acknowledged += expected;
            assert_eq!(offered.caught_up, acknowledged == records);
        }
        let bootstrap = p.lab.bootstrap(receiver);
        let session = Session::open(&bootstrap).unwrap();
        let store = Store::open(&session, &tables[healthy].1[receiver], tables[healthy].0).unwrap();
        let retained: BTreeSet<_> = store.inventory(&session).unwrap().copied().collect();
        assert!(retained.is_subset(&expected));
        stored = retained.len();
        assert_eq!(stored, acknowledged);
        if turn == 5 {
            assert_eq!(retained, expected);
        }
        drop(store);
        drop(session);
        assert!(!broken.join("table.checkpoint").exists());
        assert_eq!(
            std::fs::read(broken.join("table.astream")).unwrap(),
            original
        );
    }
    assert_eq!(acknowledged, records);
    assert!(refused > 0 && refused < 6);
    observed[0] &= i64::from(stored == records);
    observed[1] &= i64::from(acknowledged == records);

    // A restarted caller may lose its volatile cursors. It visits the damaged
    // table again, then the already repaired one without duplicating records.
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    for selected in [damaged, healthy] {
        p.round(
            &endpoint(initiator),
            &endpoint(1 - initiator),
            &mut ra,
            &mut rb,
        );
        let report = ra.application().unwrap();
        for report in [report, rb.application().unwrap()] {
            assert_eq!(report.table, Some(tables[selected].0.digest()));
            assert_eq!((report.sent, report.received), (0, 0));
            assert_eq!(report.deferred, usize::from(selected == damaged));
            if selected == damaged {
                observed[3] |= i64::from(report.sent > 0 || report.received > 0);
            }
        }
        let authenticated = ra.actor().is_some() && rb.actor().is_some();
        assert!(authenticated);
        observed[2] &= i64::from(authenticated);
    }
    let bootstrap = p.lab.bootstrap(receiver);
    let session = Session::open(&bootstrap).unwrap();
    let store = Store::open(&session, &tables[healthy].1[receiver], tables[healthy].0).unwrap();
    assert_eq!(
        store
            .inventory(&session)
            .unwrap()
            .copied()
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert!(!broken.join("table.checkpoint").exists());
    assert_eq!(
        std::fs::read(broken.join("table.astream")).unwrap(),
        original
    );
    observed
}

struct LostAck {
    input: Cursor<Vec<u8>>,
}
impl Read for LostAck {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for LostAck {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "lost acknowledgement",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct LostReceipt {
    stream: std::os::unix::net::UnixStream,
    receipt: Option<Digest>,
    failed: bool,
}
impl Read for LostReceipt {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.read(bytes)
    }
}
impl Write for LostReceipt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // A receipt is one write: the record's id, then its verdict.
        if self
            .receipt
            .as_ref()
            .is_some_and(|id| bytes.starts_with(id))
        {
            self.failed = true;
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "lost receipt"));
        }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn interrupted_application_reports_durable_ingestion_without_a_receipt() {
    let pair = Pair::new(false, 1);
    let id = {
        let bootstrap = pair.lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let store = Store::observe(&session, &pair.paths[0], pair.scope).unwrap();
        *store.inventory(&session).unwrap().next().unwrap()
    };
    let (sender, receiver) = (pair.replica(0), pair.replica(1));
    let exchange = |lose_receipt: bool, repairs: &mut [Repair; 2]| {
        let (mut left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        for stream in [&left, &right] {
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
        }
        let (sender, receiver) = (&sender, &receiver);
        let (client, server) = repairs.split_at_mut(1);
        thread::scope(|threads| {
            let worker = threads.spawn(move || {
                let mut stream = LostReceipt {
                    stream: right,
                    receipt: lose_receipt.then_some(id),
                    failed: false,
                };
                let result =
                    receiver.exchange(&mut stream, sender.actor, Side::Responder, &mut server[0]);
                (result, stream.failed)
            });
            let result =
                sender.exchange(&mut left, receiver.actor, Side::Initiator, &mut client[0]);
            drop(left);
            let (received, failed) = worker.join().unwrap();
            ([result, received], failed)
        })
    };
    let mut repairs = [Repair::default(), Repair::default()];
    let (failed, receipt_lost) = exchange(true, &mut repairs);
    assert!(receipt_lost);
    assert!(failed.iter().all(Result::is_err));
    assert_eq!(pair.count(1), 1);
    let received = repairs[1]
        .interrupted
        .expect("failed exchange must retain observed ingestion");
    assert_eq!(received.table, Some(pair.scope.digest()));
    assert_eq!(received.received, 1);
    assert_eq!(received.sent, 0);
    assert_eq!(repairs[0].interrupted.unwrap().sent, 0);

    let (retried, receipt_lost) = exchange(false, &mut repairs);
    assert!(!receipt_lost);
    assert!(retried.iter().all(Result::is_ok), "{retried:?}");
    assert!(repairs.iter().all(|repair| repair.interrupted.is_none()));
    assert_eq!(pair.count(1), 1);
    assert_eq!(retried[1].as_ref().unwrap().received, 0);
}

#[test]
fn failed_table_exchange_names_its_table_and_stage() {
    let pair = Pair::new(false, 1);
    let receiver = pair.replica(1);
    let sender = pair.replica(0);
    let before = std::fs::read(pair.paths[1].join("table.astream")).unwrap();
    let mut repair = Repair::default();
    let error = receiver
        .round(sender.actor)
        .unwrap()
        .table(
            &mut LostAck {
                input: Cursor::new(Vec::new()),
            },
            sender.actor,
            Side::Initiator,
            pair.scope.digest(),
            &mut repair,
            &mut Report::default(),
        )
        .unwrap_err();
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(&aterm_codec::hex::encode_lower(&pair.scope.digest())));
    assert!(diagnostic.contains("conflicts"));
    assert!(diagnostic.contains("lost acknowledgement"));
    assert_eq!(
        std::fs::read(pair.paths[1].join("table.astream")).unwrap(),
        before
    );
    assert_eq!(pair.count(1), 0);

    let (mut left, mut right) = (PeerRepair::default(), PeerRepair::default());
    pair.round(
        &pair.endpoint(0, true),
        &pair.endpoint(1, true),
        &mut left,
        &mut right,
    );
    assert_eq!(pair.count(1), 1);
    assert!(
        [left, right]
            .iter()
            .all(|repair| repair.application_error().is_none())
    );
}

#[test]
fn empty_batches_recheck_authority_without_reopening_unrequested_data() {
    let pair = Pair::new(false, 1);
    let sender = pair.replica(0);
    let peer = pair.replica(1).actor;
    let round = sender.round(peer).unwrap();
    let table = pair.scope.digest();
    round.active_table.set(Some(table));
    let inventory = round
        .inventory(peer, table, &mut None, &mut None)
        .unwrap()
        .unwrap();
    let record = *inventory.known.first().unwrap();
    let original = std::fs::read(pair.paths[0].join("table.astream")).unwrap();
    let held = pair.lab._home.path().join("held-table-a");
    std::fs::rename(&pair.paths[0], &held).unwrap();

    let mut output = Vec::new();
    round
        .send_batch(
            &mut Cursor::new(&mut output),
            peer,
            |session| {
                round.prepare_batch(session, table, &[] as &[Digest], |_, _, _| {
                    panic!("an empty batch encodes no table records")
                })
            },
            BatchKind::Operation,
            &mut Report::default(),
        )
        .expect("an empty batch needs no unrequested table data");
    assert_eq!(output, 0u32.to_le_bytes());
    let mut output = Vec::new();
    assert!(
        round
            .send_batch(
                &mut Cursor::new(&mut output),
                peer,
                |session| {
                    round.prepare_batch(session, table, &[record], |_, _, _| {
                        panic!("unavailable table data cannot supply a requested record")
                    })
                },
                BatchKind::Operation,
                &mut Report::default(),
            )
            .is_err()
    );
    assert!(output.is_empty(), "unavailable data emits no frame");

    std::fs::rename(&held, &pair.paths[0]).unwrap();
    assert_eq!(
        std::fs::read(pair.paths[0].join("table.astream")).unwrap(),
        original
    );
    assert_eq!((pair.count(0), pair.count(1)), (1, 0));
    let (mut left, mut right) = (PeerRepair::default(), PeerRepair::default());
    pair.round(
        &pair.endpoint(0, true),
        &pair.endpoint(1, true),
        &mut left,
        &mut right,
    );
    assert_eq!(pair.count(1), 1);
    pair.retire_b();
    let mut output = Vec::new();
    assert!(
        round
            .send_batch(
                &mut Cursor::new(&mut output),
                peer,
                |_| panic!("a retired peer cannot prepare an outgoing batch"),
                BatchKind::Operation,
                &mut Report::default(),
            )
            .is_err()
    );
    assert!(output.is_empty(), "a retired peer receives no frame");
}

fn lost_acknowledgement_retries_the_same_durable_record_once(retry_ack: bool) -> (i64, i64, i64) {
    let p = Pair::new(false, 1);
    let a = p.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let store = Store::open(&session, &p.paths[0], p.scope).unwrap();
    let id = *store.inventory(&session).unwrap().next().unwrap();
    let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
    let actor = session.actor();
    drop(store);
    drop(session);
    let b = p.replica(1);
    let mut batch = 1u32.to_le_bytes().to_vec();
    batch.extend_from_slice(&(wire.len() as u32).to_le_bytes());
    batch.extend_from_slice(&wire);
    let mut failing = LostAck {
        input: Cursor::new(batch.clone()),
    };
    let mut interrupted = Report::default();
    assert!(
        b.round(actor)
            .unwrap()
            .receive_batch(
                &mut failing,
                actor,
                p.scope.digest(),
                BatchKind::Operation,
                &mut Repair::default(),
                &mut interrupted
            )
            .is_err()
    );
    assert_eq!(p.count(1), 1);
    assert_eq!(
        interrupted.received, 1,
        "durable ingestion remains observed when its receipt cannot be sent"
    );
    if !retry_ack {
        return (p.count(1) as i64, 0, 0);
    }
    let mut retry = Cursor::new(batch);
    let mut report = Report::default();
    b.round(actor)
        .unwrap()
        .receive_batch(
            &mut retry,
            actor,
            p.scope.digest(),
            BatchKind::Operation,
            &mut Repair::default(),
            &mut report,
        )
        .unwrap();
    assert_eq!(report.received, 1);
    assert_eq!(p.count(1), 1);
    (p.count(1) as i64, report.received as i64, 0)
}

#[test]
fn wire_bounds_and_fair_rotation_do_not_depend_on_acknowledgements() {
    let mut huge = Cursor::new((MAX_RECORDS as u32 + 1).to_le_bytes().to_vec());
    assert!(receive_ids::<32>(&mut huge, MAX_RECORDS).is_err());
    let mut duplicate = 2u32.to_le_bytes().to_vec();
    duplicate.extend_from_slice(&[1; 64]);
    assert!(receive_ids::<32>(&mut Cursor::new(duplicate), 2).is_err());
    let ids: BTreeSet<_> = (1..=20).map(|n| [n; 32]).collect();
    let first: Vec<_> = cycle(&ids, None).take(BATCH).copied().collect();
    let next: Vec<_> = cycle(&ids, first.last().copied())
        .take(BATCH)
        .copied()
        .collect();
    assert_eq!(next[0], [17; 32]);
    assert!(
        next.contains(&[1; 32]),
        "deferred records are revisited after wraparound"
    );
}

/// A round catches the peer up on a table only once the peer lacks nothing of it this side
/// offered: a round that leaves some for a later one (more than a batch) has not, so a push never
/// reads a peer reached that may lack the pushed row. The side that offered nothing the peer
/// lacked is caught up at once.
///
/// NEGATIVE CONTROL: drop the `whole` term (`report.caught_up` whenever the peer accepted the
/// batch sent), and the first round, which leaves four operations for the next, reads as caught
/// up.
#[test]
fn a_round_catches_the_peer_up_only_once_it_lacks_nothing_of_the_table() {
    let records = BATCH + 4;
    let p = Pair::new(false, records);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    let first = ra.application().expect("a completed application round");
    assert_eq!((first.sent, p.count(1)), (BATCH, BATCH));
    assert!(!first.caught_up, "four operations wait for the next round");
    assert!(
        rb.application().expect("the responder's round").caught_up,
        "the peer offered nothing this side lacked"
    );
    p.round(&a, &b, &mut ra, &mut rb);
    let second = ra.application().expect("a completed application round");
    assert_eq!((second.sent, p.count(1)), (records - BATCH, records));
    assert!(second.caught_up);
}

/// A push's table goes first: while the initiator's push names a table both sides hold, its round
/// repairs that one, though the turn would take the other.
///
/// NEGATIVE CONTROL: make the initiator's choice ignore `first` (only `cycle`), and the round
/// repairs the lower table, the turn's.
#[test]
fn a_pushed_table_is_repaired_before_the_tables_turn() {
    let p = Pair::new(false, 1);
    let other_paths = [
        p.lab._home.path().join("other-a"),
        p.lab._home.path().join("other-b"),
    ];
    let bootstrap = p.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut other = Store::create(&session, &other_paths[0]).unwrap();
    other.commit_catalog(&session, b"pushed edit").unwrap();
    let other_scope = other.scope();
    drop(other);
    drop(session);
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    Store::join(&session, &other_paths[1], other_scope).unwrap();
    drop(session);
    let tables = [(p.scope, p.paths.clone()), (other_scope, other_paths)];
    let endpoint = |node: usize| {
        Endpoint::from_bootstrap(&p.lab.bootstrap(node))
            .unwrap()
            .unwrap()
            .with_tables(
                p.lab.paths[node].clone(),
                tables
                    .iter()
                    .map(|(scope, paths)| (*scope, paths[node].clone()))
                    .collect(),
            )
            .unwrap()
    };
    let (lower, higher) = if p.scope.digest() < other_scope.digest() {
        (p.scope.digest(), other_scope.digest())
    } else {
        (other_scope.digest(), p.scope.digest())
    };
    assert_eq!(
        cycle(&BTreeSet::from([lower, higher]), None).next(),
        Some(&lower),
        "the turn takes the lower table first"
    );
    let mut a = endpoint(0);
    a.prefer(std::sync::Arc::new(std::sync::Mutex::new(Some(higher))));
    let b = endpoint(1);
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    let report = ra.application().expect("a completed application round");
    assert_eq!(report.table, Some(higher), "the pushed table went first");
    assert_eq!(report.sent, 1);
}

/// Execute finite model transitions against actual TLS, custody and durable logs.
pub fn repair_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    if action == "DamagedNeighbor" {
        let [stored, acknowledged, control, neighbor_acknowledged] =
            a_damaged_neighbor_does_not_starve_healthy_table_repair();
        let mut next = state.clone();
        next.insert("scenario", 8);
        next.insert("stored", stored);
        next.insert("acknowledged", acknowledged);
        next.insert("control", control);
        next.insert("neighbor_acknowledged", neighbor_acknowledged);
        return next;
    }
    let (scenario, (stored, acknowledged, control)) = match action {
        "NewMember" | "LostKey" => {
            let p = Pair::new(true, 1);
            let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
            let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
            p.round(&a, &b, &mut ra, &mut rb);
            let sent = ra.application().unwrap().sent;
            if action == "LostKey" {
                let history = p.lab.paths[1].dir.join("commons/history");
                for entry in std::fs::read_dir(history).unwrap() {
                    std::fs::remove_file(entry.unwrap().path()).unwrap();
                }
                p.round(&a, &b, &mut ra, &mut rb);
                assert_eq!(rb.application().unwrap().keys_received, 1);
            }
            (
                if action == "NewMember" { 1 } else { 2 },
                (
                    p.count(1) as i64,
                    sent as i64,
                    i64::from(ra.actor().is_some() && rb.actor().is_some()),
                ),
            )
        }
        "Legacy" => (
            3,
            old_peers_still_complete_control_repair_in_both_directions(),
        ),
        "Retired" => (
            4,
            revocation_after_endpoint_creation_withholds_application_data(),
        ),
        "Damaged" => (
            5,
            damaged_table_is_unavailable_and_never_reset_or_acknowledged(),
        ),
        "LostAck" => (
            6,
            lost_acknowledgement_retries_the_same_durable_record_once(false),
        ),
        "Retry" => (
            7,
            lost_acknowledgement_retries_the_same_durable_record_once(true),
        ),
        _ => panic!("unknown repair transition"),
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("stored", stored);
    next.insert("acknowledged", acknowledged);
    next.insert("control", control);
    next.insert("neighbor_acknowledged", 0);
    next
}

#[test]
fn real_repair_conforms_and_receipt_without_storage_mutant_is_detected() {
    let model = aterm_models_db::table_repair_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    // Each transition's real scenario builds its own Lab, so they run on the lane's places.
    let names: Vec<&'static str> = model.actions.iter().map(|action| action.name).collect();
    let actuals = aterm_spec::fs_lane::each(&names, |name| repair_transition(&initial, name));
    for (action, actual) in model.actions.iter().zip(actuals) {
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let mut forged = initial.clone();
    forged.insert("scenario", 1);
    forged.insert("acknowledged", 1);
    forged.insert("control", 1);
    assert!(!model.successors("NewMember", &initial).contains(&forged));
    assert!(!model.check_invariant("ReceiptsRequireStorage", &forged));
    let mut starved = model.successors("DamagedNeighbor", &initial).remove(0);
    starved.insert("stored", 0);
    starved.insert("acknowledged", 0);
    assert!(!model.check_invariant("AvailableRepairPersists", &starved));
    assert!(
        !model
            .successors("DamagedNeighbor", &initial)
            .contains(&starved)
    );
    let mut unacknowledged = model.successors("DamagedNeighbor", &initial).remove(0);
    unacknowledged.insert("acknowledged", 0);
    assert!(!model.check_invariant("NeighborRepairAcknowledged", &unacknowledged));
    assert!(
        !model
            .successors("DamagedNeighbor", &initial)
            .contains(&unacknowledged)
    );
    let mut reset = model.successors("DamagedNeighbor", &initial).remove(0);
    reset.insert("neighbor_acknowledged", 1);
    assert!(!model.check_invariant("DamagedNeighborIsNotAcknowledged", &reset));
    assert!(
        !model
            .successors("DamagedNeighbor", &initial)
            .contains(&reset)
    );
}

#[test]
fn application_delivery_reads_beside_an_unrelated_source_control_writer() {
    use std::os::unix::net::UnixStream;

    let p = Pair::new(true, 1);
    let (a, b) = (p.replica(0), p.replica(1));
    let source = p.lab.bootstrap(0);
    let held = Journal::open_existing(source.journal_dir(), source.trust()).unwrap();
    let outgoing = a.round(b.actor).expect("observing a round needs no writer");
    let incoming = b.round(a.actor).unwrap();
    let (mut left, mut right) = UnixStream::pair().unwrap();
    for stream in [&left, &right] {
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
    }
    thread::scope(|threads| {
        let receiver = threads.spawn(|| {
            incoming.exchange(&mut right, a.actor, Side::Responder, &mut Repair::default())
        });
        let sent = outgoing
            .exchange(&mut left, b.actor, Side::Initiator, &mut Repair::default())
            .unwrap();
        let received = receiver.join().unwrap().unwrap();
        assert_eq!((sent.sent, received.received), (1, 1));
    });
    let destination = p.lab.bootstrap(1);
    let session = Session::observe(&destination).unwrap();
    let store = Store::open(&session, &p.paths[1], p.scope).unwrap();
    let records = store.catalog(&session).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payload(), b"account-metadata");
    drop(held);
}

#[test]
fn every_application_io_drops_locks_and_rotation_invalidates_a_round() {
    use std::os::unix::net::UnixStream;
    struct Unlocked {
        stream: UnixStream,
        dir: PathBuf,
        trust: Trust,
    }
    impl Unlocked {
        fn check(&self) {
            drop(
                Journal::open_existing(&self.dir, self.trust)
                    .expect("application I/O held control lock"),
            );
        }
    }
    impl Read for Unlocked {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.check();
            self.stream.read(bytes)
        }
    }
    impl Write for Unlocked {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.check();
            self.stream.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.check();
            self.stream.flush()
        }
    }
    let p = Pair::new(true, 1);
    let (a, b) = (p.replica(0), p.replica(1));
    let (sa, sb) = (p.lab.bootstrap(0), p.lab.bootstrap(1));
    let (left, right) = UnixStream::pair().unwrap();
    for stream in [&left, &right] {
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
    }
    let mut left = Unlocked {
        stream: left,
        dir: sa.journal_dir().to_owned(),
        trust: sa.trust(),
    };
    let mut right = Unlocked {
        stream: right,
        dir: sb.journal_dir().to_owned(),
        trust: sb.trust(),
    };
    thread::scope(|threads| {
        let receiver = threads
            .spawn(|| b.exchange(&mut right, a.actor, Side::Responder, &mut Repair::default()));
        a.exchange(&mut left, b.actor, Side::Initiator, &mut Repair::default())
            .unwrap();
        receiver.join().unwrap().unwrap();
    });
    assert_eq!(p.count(1), 1);
    let round = a.round(b.actor).unwrap();
    p.lab.rotate(&[Role::Writer, Role::Reader]);
    let mut empty = Cursor::new(Vec::new());
    assert!(
        round
            .exchange(&mut empty, b.actor, Side::Initiator, &mut Repair::default())
            .is_err()
    );
    assert!(
        empty.into_inner().is_empty(),
        "no probe or inventory from a stale observation"
    );
}

#[test]
fn blocking_reads_cannot_turn_a_changed_roster_into_a_successful_round() {
    struct Rotating<'a> {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
        rotate: Option<Box<dyn FnOnce() + 'a>>,
    }
    impl Read for Rotating<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if let Some(rotate) = self.rotate.take() {
                rotate();
            }
            self.input.read(bytes)
        }
    }
    impl Write for Rotating<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    for lane in 0..3 {
        let keys = lane == 1;
        let kind = if lane == 2 {
            BatchKind::Conflict
        } else {
            BatchKind::Operation
        };
        let p = Pair::new(false, 1);
        let a = p.lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let store = Store::open(&session, &p.paths[0], p.scope).unwrap();
        let id = *store.inventory(&session).unwrap().next().unwrap();
        let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
        let source = *session.authenticate(p.scope, &wire).unwrap().roster();
        let actor = session.actor();
        let target = p.replica(1);
        let payload = if keys {
            session
                .handoffs(target.actor, &[source])
                .unwrap()
                .pop()
                .unwrap()
                .unwrap()
        } else if lane == 2 {
            let mut second = wire.clone();
            let end = second.len() - 64;
            second[end - 1] ^= 1;
            crate::table::tests::resign(&a, &mut second);
            crate::table::conflict::Conflict::from_pair(&session, p.scope, &wire, &second)
                .unwrap()
                .wire()
                .to_vec()
        } else {
            wire
        };
        drop(store);
        drop(session);
        let mut input = if keys {
            vec![1]
        } else {
            1u32.to_le_bytes().to_vec()
        };
        input.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        input.extend_from_slice(&payload);
        let round = target.round(actor).unwrap();
        let mut stream = Rotating {
            input: Cursor::new(input),
            output: Vec::new(),
            rotate: Some(Box::new(|| p.lab.rotate(&[Role::Writer, Role::Writer]))),
        };
        let result = if keys {
            round.receive_keys(
                &mut stream,
                actor,
                &BTreeSet::from([source]),
                &mut Report::default(),
            )
        } else {
            round.receive_batch(
                &mut stream,
                actor,
                p.scope.digest(),
                kind,
                &mut Repair::default(),
                &mut Report::default(),
            )
        };
        assert!(
            result.is_err(),
            "a changed roster must abort after a blocking read"
        );
        assert!(
            stream.output.is_empty(),
            "no receipt follows failed authorization"
        );
        assert_eq!(p.count(1), 0);
    }
}

#[test]
fn fresh_inbound_connections_continue_past_the_first_history_window() {
    // Every historical epoch initially excludes B. More than one key window
    // is needed even though each incoming worker starts with fresh Repair.
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Reader], 1);
    let paths = [
        lab._home.path().join("table-a"),
        lab._home.path().join("table-b"),
    ];
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let mut store = Store::create(&session, &paths[0]).unwrap();
    let scope = store.scope();
    store
        .commit_catalog(&session, b"historical record")
        .unwrap();
    drop(store);
    drop(session);
    for _ in 0..BATCH {
        lab.rotate(&[Role::Writer]);
    }
    lab.admit(&[Role::Writer, Role::Reader]);
    let b = lab.bootstrap(1);
    let session = Session::open(&b).unwrap();
    Store::join(&session, &paths[1], scope).unwrap();
    drop(session);
    let p = Pair { lab, scope, paths };
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let mut outgoing = PeerRepair::default();
    let mut first = 0;
    let mut later = 0;
    for round in 0..4 {
        let mut incoming = PeerRepair::default();
        p.round(&a, &b, &mut outgoing, &mut incoming);
        let keys = incoming
            .application()
            .unwrap_or_else(|| panic!("{incoming:?}"))
            .keys_received;
        if round == 0 {
            first = keys;
        } else {
            later += keys;
        }
    }
    assert!(
        first > 0 && later > 0,
        "inbound repair must scan beyond its first window"
    );
    assert_eq!(p.count(1), 1);
    let b = p.lab.bootstrap(1);
    assert!(
        Session::open(&b)
            .unwrap()
            .history_needs(None, usize::MAX)
            .unwrap()
            .0
            .is_empty()
    );
}

#[test]
fn lost_conflict_ack_retries_the_original_durable_certificate() {
    let p = Pair::new(false, 1);
    let a = p.lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let store = Store::open(&session, &p.paths[0], p.scope).unwrap();
    let id = *store.inventory(&session).unwrap().next().unwrap();
    let first = store.wire(&session, &id).unwrap().unwrap();
    let mut second = first.to_vec();
    let end = second.len() - 64;
    second[end - 1] ^= 1;
    crate::table::tests::resign(&a, &mut second);
    let certificate =
        crate::table::conflict::Conflict::from_pair(&session, p.scope, first, &second).unwrap();
    let wire = certificate.wire().to_vec();
    let actor = session.actor();
    drop((store, session));
    let b = p.replica(1);
    let mut bytes = 1u32.to_le_bytes().to_vec();
    bytes.extend_from_slice(&(wire.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&wire);
    let mut lost = LostAck {
        input: Cursor::new(bytes.clone()),
    };
    let mut interrupted = Report::default();
    assert!(
        b.round(actor)
            .unwrap()
            .receive_batch(
                &mut lost,
                actor,
                p.scope.digest(),
                BatchKind::Conflict,
                &mut Repair::default(),
                &mut interrupted
            )
            .is_err()
    );
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &p.paths[1], p.scope).unwrap();
    assert_eq!(store.conflict_inventory(&session).unwrap().len(), 1);
    assert_eq!(
        interrupted.conflicts_received, 1,
        "durable conflict ingestion remains observed after a lost receipt"
    );
    assert_eq!(store.inventory(&session).unwrap().count(), 0);
    let receipt = store.ingest_conflict(&session, &wire).unwrap();
    drop((store, session));
    let mut retry = Cursor::new(bytes);
    let mut report = Report::default();
    b.round(actor)
        .unwrap()
        .receive_batch(
            &mut retry,
            actor,
            p.scope.digest(),
            BatchKind::Conflict,
            &mut Repair::default(),
            &mut report,
        )
        .unwrap();
    assert_eq!(report.conflicts_received, 1);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &p.paths[1], p.scope).unwrap();
    assert_eq!(store.ingest_conflict(&session, &wire).unwrap(), receipt);
}

#[test]
fn application_greetings_bind_every_control_byte_in_both_directions() {
    let observation = [37; 64];
    for tag in [PROBE, READY] {
        let frame = [tag, observation.as_slice()].concat();
        assert_eq!(validate_probe(&frame, tag, &observation), Ok(()));
        for offset in 0..observation.len() {
            let mut changed = frame.clone();
            changed[tag.len() + offset] ^= 1;
            assert_eq!(
                validate_probe(&changed, tag, &observation),
                Err(ProbeMismatch::History {
                    head_matches: offset >= 32,
                    basis_matches: offset < 32,
                })
            );
        }
        let mut changed = frame.clone();
        changed[tag.len()] ^= 1;
        changed[tag.len() + 32] ^= 1;
        assert_eq!(
            validate_probe(&changed, tag, &observation),
            Err(ProbeMismatch::History {
                head_matches: false,
                basis_matches: false
            })
        );
    }
}

#[test]
fn unsupported_or_malformed_greetings_are_not_control_history_observations() {
    let observation = [37; 64];
    for tag in [PROBE, READY] {
        for wrong in [b"ACTP1", b"ACTA1", b"ACTP2", b"ACTA2", b"ACTP3", b"ACTA3"] {
            if wrong.as_slice() == tag {
                continue;
            }
            let frame = [wrong.as_slice(), observation.as_slice()].concat();
            assert_eq!(
                validate_probe(&frame, tag, &observation),
                Err(ProbeMismatch::Protocol)
            );
        }
        let frame = [tag, observation.as_slice()].concat();
        for length in 0..frame.len() {
            assert_eq!(
                validate_probe(&frame[..length], tag, &observation),
                Err(ProbeMismatch::Malformed)
            );
        }
        let mut oversized = frame;
        oversized.push(0);
        assert_eq!(
            validate_probe(&oversized, tag, &observation),
            Err(ProbeMismatch::Malformed)
        );
    }
}

#[test]
fn older_application_frames_cannot_bypass_certificate_exchange() {
    let p = Pair::new(false, 0);
    let replica = p.replica(0);
    let peer = p.replica(1).actor;
    let round = replica.round(peer).unwrap();
    for (old, expected) in [
        (b"ACTP1", PROBE),
        (b"ACTA1", READY),
        (b"ACTP2", PROBE),
        (b"ACTA2", READY),
    ] {
        let mut frame = Vec::new();
        control::send_frame(&mut frame, &[old.as_slice(), &round.observation].concat()).unwrap();
        assert!(
            round
                .receive_probe(&mut Cursor::new(frame), expected)
                .is_err()
        );
    }
    let actors = BTreeSet::from([[1; 48], [2; 48]]);
    let mut frame = Vec::new();
    send_inventory(&mut frame, Some(&actors), MAX_RECORDS).unwrap();
    assert_eq!(
        receive_inventory::<48>(&mut Cursor::new(frame), MAX_RECORDS).unwrap(),
        Some(actors)
    );
}

#[test]
fn proof_inventory_carries_both_retained_source_pools_without_unbounded_allocation() {
    let proofs: BTreeSet<ProofKey> = (0..MAX_PROOFS as u32)
        .map(|n| {
            let mut key = [0; 81];
            key[77..].copy_from_slice(&n.to_be_bytes());
            key
        })
        .collect();
    let mut wire = Vec::new();
    send_inventory(&mut wire, Some(&proofs), MAX_PROOFS).unwrap();
    assert_eq!(
        receive_inventory::<81>(&mut Cursor::new(wire), MAX_PROOFS).unwrap(),
        Some(proofs)
    );
    let mut oversized = vec![1];
    oversized.extend_from_slice(&(MAX_PROOFS as u32 + 1).to_le_bytes());
    assert!(receive_inventory::<81>(&mut Cursor::new(oversized), MAX_PROOFS).is_err());
}

#[test]
fn completed_rounds_trade_signed_versions_and_a_0_145_peer_reads_as_saying_nothing() {
    use aterm_secrets::commons::hearing::{Said, Version};
    let p = Pair::new(false, 1);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &b, &mut ra, &mut rb);
    assert!(ra.application().is_some() && rb.application().is_some());
    assert_eq!(ra.said(), Said::Version(Version::own()));
    assert_eq!(rb.said(), Said::Version(Version::own()));
    // A 0.145 peer, in either seat: its round completes as before, it trades nothing, and
    // this side reads that as a peer that said nothing (the C26 gate holds for it).
    let mut old = p.endpoint(1, true);
    old.legacy = true;
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&a, &old, &mut ra, &mut rb);
    assert!(ra.application().is_some() && rb.application().is_some());
    assert_eq!(ra.said(), Said::Nothing);
    assert_eq!(rb.said(), Said::Unknown);
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&old, &a, &mut rb, &mut ra);
    assert!(ra.application().is_some() && rb.application().is_some());
    assert_eq!(ra.said(), Said::Nothing);
    // Negative control: without a completed round nothing is traded or concluded.
    let (bare_a, bare_b) = (p.endpoint(0, false), p.endpoint(1, false));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&bare_a, &bare_b, &mut ra, &mut rb);
    assert_eq!((ra.said(), rb.said()), (Said::Unknown, Said::Unknown));
}
