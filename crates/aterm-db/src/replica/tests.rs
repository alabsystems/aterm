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
        // These tests need each round to complete: on a loaded machine one outlasts the
        // product's 30 s deadline, and its application report then reads as missing.
        let endpoint = Endpoint::from_bootstrap(&self.lab.bootstrap(n))
            .unwrap()
            .unwrap()
            .within(Duration::from_secs(60));
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

#[test]
fn tls_fetches_historical_keys_then_repairs_pages_and_restarts() {
    let p = Pair::new(true, 35);
    let (a, b) = (p.endpoint(0, true), p.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    for _ in 0..3 {
        p.round(&a, &b, &mut ra, &mut rb);
    }
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
    assert!(
        b.round(actor)
            .unwrap()
            .receive_batch(
                &mut failing,
                actor,
                p.scope.digest(),
                BatchKind::Operation,
                &mut Report::default()
            )
            .is_err()
    );
    assert_eq!(p.count(1), 1);
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

/// Execute finite model transitions against actual TLS, custody and durable logs.
pub fn repair_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
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
    for action in &model.actions {
        let actual = repair_transition(&initial, action.name);
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
        let keys = incoming.application().unwrap().keys_received;
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
    assert!(
        b.round(actor)
            .unwrap()
            .receive_batch(
                &mut lost,
                actor,
                p.scope.digest(),
                BatchKind::Conflict,
                &mut Report::default()
            )
            .is_err()
    );
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &p.paths[1], p.scope).unwrap();
    assert_eq!(store.conflict_inventory(&session).unwrap().len(), 1);
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
            &mut report,
        )
        .unwrap();
    assert_eq!(report.conflicts_received, 1);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &p.paths[1], p.scope).unwrap();
    assert_eq!(store.ingest_conflict(&session, &wire).unwrap(), receipt);
}

#[test]
fn version_one_application_frames_cannot_bypass_certificate_exchange() {
    let p = Pair::new(false, 0);
    let replica = p.replica(0);
    let peer = p.replica(1).actor;
    let round = replica.round(peer).unwrap();
    for (old, expected) in [(b"ACTP1", PROBE), (b"ACTA1", READY)] {
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
    send_inventory(&mut frame, Some(&actors)).unwrap();
    assert_eq!(
        receive_inventory::<48>(&mut Cursor::new(frame)).unwrap(),
        Some(actors)
    );
}
