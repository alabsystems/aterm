// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::frontier::Metadata;
use aterm_secrets::{
    client,
    crossing::RecordsBound,
    membership_log::{Body, Entrant, Log, Record, encode_records},
};

#[test]
fn frontier_metadata_is_fixed_exact_and_independently_pinned() {
    let lab = Lab::new();
    let metadata = Metadata {
        request: lab.requests[0].clone(),
        bound: RecordsBound::here(false),
        flags: 0,
    };
    let wire = metadata.encode(lab.trust);
    assert_eq!(wire.len(), HELLO_FIXED + 6);
    let decoded = Metadata::decode(lab.trust, &wire).unwrap();
    assert_eq!(decoded.request.bytes(), metadata.request.bytes());
    assert_eq!(decoded.bound, metadata.bound);
    for length in 0..wire.len() {
        assert!(Metadata::decode(lab.trust, &wire[..length]).is_err());
    }
    let mut extended = wire.clone();
    extended.push(0);
    assert!(Metadata::decode(lab.trust, &extended).is_err());
    for at in [0, 4, 20, 52, HELLO_FIXED + 4, HELLO_FIXED + 5] {
        let mut damaged = wire.clone();
        damaged[at] ^= 128;
        assert!(Metadata::decode(lab.trust, &damaged).is_err(), "byte {at}");
    }
    let mut zero = wire;
    zero[HELLO_FIXED..HELLO_FIXED + 4].fill(0);
    assert!(Metadata::decode(lab.trust, &zero).is_err());
}

#[test]
fn frontier_refuses_missing_reflected_and_incompatible_identity_before_evidence() {
    let lab = Lab::new();
    let endpoint = lab.endpoint(0);
    let remote = lab.endpoint(1);
    let valid_proof = NodeProof {
        node: remote.identity.node().into(),
        key: remote.identity.key(),
    };
    let reflected = NodeProof {
        node: endpoint.identity.node().into(),
        key: endpoint.identity.key(),
    };
    for (proof, bound) in [
        (None, RecordsBound::here(false)),
        (Some(&reflected), RecordsBound::here(false)),
        (
            Some(&valid_proof),
            RecordsBound::of(u32::MAX as usize, true),
        ),
    ] {
        let peer = Metadata {
            request: lab.requests[1].clone(),
            bound,
            flags: 0,
        };
        let mut input = Vec::new();
        send_frame(&mut input, &peer.encode(lab.trust)).unwrap();
        let metadata_end = input.len() as u64;
        send_frame(&mut input, b"evidence must remain unread").unwrap();
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let mut repair = Repair::default();
        assert!(
            endpoint
                .connected_until(
                    &mut stream,
                    proof,
                    Side::Initiator,
                    &mut repair,
                    Instant::now() + EXCHANGE,
                    Protocol::Frontier
                )
                .is_err()
        );
        assert_eq!(stream.input.position(), metadata_end);
        assert_eq!(stream.output.len(), 4 + HELLO_FIXED + 6);
        assert!(repair.authenticated_actor().is_none());
    }
}

#[test]
fn frontier_native_tls_repairs_control_with_an_independent_sealed_root() {
    let lab = Lab::new();
    let update = lab.update(lab.genesis.id(), b"frontier-control", &[]);
    let mut source = lab.journal(0);
    source.ingest(lab.genesis.bytes()).unwrap();
    source.ingest(update.bytes()).unwrap();
    drop(source);
    let (server, client, repair) = dial(&lab.endpoint(1), &lab.endpoint(0));
    assert!(server.is_ok(), "{server:?}");
    assert!(client.is_ok(), "{client:?}");
    assert!(repair.authenticated_actor().is_some());
    assert_eq!(
        lab.journal(1).inventory().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([lab.genesis.id(), update.id()])
    );
}

struct RecordsLab {
    lab: Lab,
    paths: [client::Paths; 2],
    endpoints: [Endpoint; 2],
    log: Log,
}

impl RecordsLab {
    fn new() -> Self {
        let lab = Lab::new();
        let entrants = lab
            .requests
            .iter()
            .enumerate()
            .map(|(n, request)| Entrant {
                request: request.clone(),
                node_key: *lab.endpoint(n).identity.key().bytes(),
                name: format!("m{}", n + 1),
                label: format!("m{}", n + 1),
            })
            .collect();
        let genesis = Record::found(&ROOT, entrants, 1).unwrap();
        let log = Log::new(genesis.clone()).unwrap();
        assert_ne!(
            *log.network(),
            lab.trust.genesis,
            "membership and sealed control have independent roots"
        );
        let paths: [client::Paths; 2] = std::array::from_fn(|n| {
            let paths = client::Paths::for_home(&lab.root.path().join(format!("member-{n}")));
            let pin = aterm_codec::base64::encode(
                &aterm_secrets::envelope::signer_public(&ROOT).unwrap(),
            )
            .unwrap();
            client::configure_log_member(
                &paths,
                &format!("{}#{pin}", lab.root.path().join("team.vault").display()),
                &format!("m{}", n + 1),
            )
            .unwrap();
            client::save_records(&paths, genesis.bytes()).unwrap();
            paths
        });
        let endpoints: [Endpoint; 2] = std::array::from_fn(|n| {
            Endpoint::configured_log(
                &lab.path(n),
                lab.trust,
                &lab.requests[n],
                Arc::new(log.clone()),
                &[n as u8 + 31; 32],
            )
            .unwrap()
            .with_membership_paths(paths[n].clone())
        });
        Self {
            lab,
            paths,
            endpoints,
            log,
        }
    }

    fn names(&self, member: usize, count: u64) -> Log {
        let mut log = self.log.clone();
        for n in 0..count {
            let record = log
                .draft(
                    &self.lab.requests[member].binding().actor(),
                    n + 2,
                    Body::Name {
                        name: format!("branch-{member}-{n}"),
                    },
                )
                .sign(&[member as u8 + 31; 32], [member as u8 + 3; 16])
                .unwrap();
            log.insert(record).unwrap();
        }
        let wire = encode_records(log.records());
        if log.held_count() > aterm_secrets::membership_log::LEGACY_RECORDS {
            // Construct future-budget custody without relaxing the public legacy wire decoder.
            aterm_secrets::fsx::write_atomic(
                &self.paths[member].records_file().with_extension("crossed"),
                b"crossed\n",
                0o600,
            )
            .unwrap();
            aterm_secrets::fsx::write_atomic(&self.paths[member].records_file(), &wire, 0o600)
                .unwrap();
        } else {
            client::save_records(&self.paths[member], &wire).unwrap();
        }
        log
    }
}

#[test]
fn frontier_typed_local_evidence_keeps_root_and_capacity_checks() {
    use aterm_secrets::membership_log::RecordBudgets;
    let fixture = RecordsLab::new();
    for field in 0..3 {
        let mut trust = fixture.lab.trust;
        match field {
            0 => trust.network[0] ^= 1,
            1 => trust.authority[0] ^= 1,
            _ => trust.genesis = [0; 32],
        }
        assert!(
            Endpoint::configured_log(
                &fixture.lab.path(0),
                trust,
                &fixture.lab.requests[0],
                Arc::new(fixture.log.clone()),
                &[31; 32],
            )
            .is_err()
        );
    }
    let large = fixture.names(0, 2);
    RecordBudgets {
        held: 2,
        ..RecordBudgets::PRODUCTION
    }
    .within(|| {
        assert!(
            Endpoint::configured_log(
                &fixture.lab.path(0),
                fixture.lab.trust,
                &fixture.lab.requests[0],
                Arc::new(large),
                &[31; 32],
            )
            .is_err()
        );
    });
}

#[test]
fn frontier_native_tls_repairs_marked_history_and_preserves_its_mark_in_either_direction() {
    for source in 0..2 {
        let RecordsLab {
            lab,
            paths,
            endpoints,
            mut log,
        } = RecordsLab::new();
        for n in 0..39 {
            let record = log
                .draft(
                    &lab.requests[source].binding().actor(),
                    n + 2,
                    Body::Name {
                        name: format!("frontier-{source}-{n}"),
                    },
                )
                .sign(&[source as u8 + 31; 32], [source as u8 + 3; 16])
                .unwrap();
            log.insert(record).unwrap();
        }
        client::save_records(&paths[source], &encode_records(log.records())).unwrap();
        std::fs::write(
            paths[source].records_file().with_extension("crossed"),
            b"crossed\n",
        )
        .unwrap();
        let (server, client_result, repair) = dial(&endpoints[1], &endpoints[0]);
        assert!(server.is_ok(), "source {source}: {server:?}");
        assert!(client_result.is_ok(), "source {source}: {client_result:?}");
        assert!(
            repair.membership_error().is_none(),
            "{:?}",
            repair.membership_error()
        );
        assert_eq!(
            client::load_records(&paths[0]).unwrap().inventory(),
            client::load_records(&paths[1]).unwrap().inventory()
        );
        assert_eq!(
            client::load_records(&paths[1 - source])
                .unwrap()
                .inventory()
                .len(),
            40
        );
        assert!(client::records_marked_crossed(&paths[1 - source]).unwrap());
    }
}

#[test]
fn frontier_native_tls_converges_equal_count_forks() {
    let fixture = RecordsLab::new();
    let left = fixture.names(0, 19);
    let right = fixture.names(1, 19);
    let expected: BTreeSet<_> = left
        .records()
        .chain(right.records())
        .map(|r| *r.id())
        .collect();
    let (server, client_result, repair) = dial(&fixture.endpoints[1], &fixture.endpoints[0]);
    assert!(
        server.is_ok() && client_result.is_ok(),
        "{server:?} {client_result:?}"
    );
    assert!(
        repair.membership_error().is_none(),
        "{:?}",
        repair.membership_error()
    );
    assert_eq!(expected.len(), 39);
    for paths in &fixture.paths {
        assert_eq!(client::load_records(paths).unwrap().inventory(), expected);
    }
}

#[test]
fn frontier_full_stores_hold_refused_records_until_capacity_changes() {
    use aterm_secrets::membership_log::RecordBudgets;
    const FULL: RecordBudgets = RecordBudgets {
        held: 2,
        ..RecordBudgets::PRODUCTION
    };
    const ROOM: RecordBudgets = RecordBudgets {
        held: 4,
        ..RecordBudgets::PRODUCTION
    };
    let mut fixture = FULL.within(|| {
        let fixture = RecordsLab::new();
        fixture.names(0, 1);
        fixture.names(1, 1);
        fixture
    });
    let round = |fixture: &RecordsLab, budgets: RecordBudgets| {
        budgets.within(|| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            thread::scope(|scope| {
                let server = scope.spawn(|| {
                    budgets.within(|| {
                        let mut repair = Repair::default();
                        let result = fixture.endpoints[1].exchange(
                            Connection::admit(listener.accept().unwrap().0, Scope::Loopback)
                                .unwrap(),
                            Side::Responder,
                            &mut repair,
                        );
                        (result, repair)
                    })
                });
                let mut repair = Repair::default();
                let result = fixture.endpoints[0].exchange(
                    Connection::connect(address, Scope::Loopback, EXCHANGE).unwrap(),
                    Side::Initiator,
                    &mut repair,
                );
                let (server, server_repair) = server.join().unwrap();
                assert!(server.is_ok() && result.is_ok(), "{server:?} {result:?}");
                [repair, server_repair]
            })
        })
    };
    for _ in 0..2 {
        for repair in round(&fixture, FULL) {
            assert!(repair.membership_error().is_some());
            assert!(
                repair
                    .membership_hint
                    .refused
                    .as_ref()
                    .and_then(|run| run.held)
                    .is_some(),
                "a complete refusal must establish a hold"
            );
            assert!(
                repair.membership_hint.cursor.is_none(),
                "a held peer must not advance through another refused prefix"
            );
        }
    }
    for endpoint in &mut fixture.endpoints {
        endpoint.records_bound = RecordsBound::of(ROOM.held, false);
    }
    for repair in round(&fixture, ROOM) {
        assert!(repair.membership_error().is_none(), "{repair:?}");
        assert!(repair.membership_hint.refused.is_none());
    }
    ROOM.within(|| {
        let left = client::load_records(&fixture.paths[0]).unwrap().inventory();
        let right = client::load_records(&fixture.paths[1]).unwrap().inventory();
        assert_eq!(left, right);
        assert_eq!(left.len(), 3);
    });
}

#[test]
fn frontier_batches_visit_every_record_with_count_and_byte_bounds() {
    use aterm_secrets::{
        SecretBytes,
        membership_log::EpochAudience,
        text::{Audience, Entry},
    };
    let fixture = RecordsLab::new();
    let mut log = fixture.names(0, 600);
    let actor = fixture.lab.requests[0].binding().actor();
    let (draft, key) = log
        .draft_epoch(&actor, 602, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&[31; 32], [3; 16]).unwrap();
    let epoch_id = *epoch.id();
    log.insert(epoch).unwrap();
    let entry = Entry {
        name: "batch-payload".into(),
        value: SecretBytes::new(vec![0x53; 512 * 1024]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    for n in 0..12 {
        let record = log
            .draft_put(&actor, 603 + n, epoch_id, &key, &entry)
            .unwrap()
            .sign(&[31; 32], [3; 16])
            .unwrap();
        log.insert(record).unwrap();
    }
    let records: Vec<_> = log.records().cloned().collect();
    let expected: BTreeSet<_> = records.iter().map(|record| *record.id()).collect();
    let mut seen = BTreeSet::new();
    let mut cursor = None;
    let mut count_limited = false;
    let mut byte_limited = false;
    for _ in 0..records.len() {
        let batch = crate::control::frontier::select_batch(&records, &mut cursor);
        assert!(!batch.is_empty());
        assert!(batch.len() <= 256);
        let bytes: usize = batch.iter().map(|record| record.bytes().len()).sum();
        assert!(bytes <= 4 * 1024 * 1024);
        count_limited |= batch.len() == 256;
        byte_limited |= batch.len() < 256 && bytes > 3 * 1024 * 1024;
        let before = seen.len();
        seen.extend(batch.iter().map(|record| *record.id()));
        assert!(
            seen.len() > before,
            "a refused prefix must not starve later records"
        );
        if seen == expected {
            break;
        }
    }
    assert_eq!(seen, expected);
    assert!(count_limited && byte_limited);
}

#[test]
fn frontier_keeps_selected_evidence_after_the_seat_record() {
    use aterm_secrets::membership_log::{EpochAudience, Evidence as Admission};
    let lab = Lab::new();
    let entrant = |n: usize| Entrant {
        request: lab.requests[n].clone(),
        node_key: *lab.endpoint(n).identity.key().bytes(),
        name: format!("m{}", n + 1),
        label: format!("m{}", n + 1),
    };
    let genesis = Record::found(&ROOT, vec![entrant(0)], 1).unwrap();
    let mut log = Log::new(genesis).unwrap();
    let actor = lab.requests[0].binding().actor();
    let (draft, key) = log
        .draft_epoch(&actor, 2, EpochAudience::Commons, 1)
        .unwrap();
    let epoch = draft.sign(&[31; 32], [3; 16]).unwrap();
    let epoch_id = *epoch.id();
    log.insert(epoch).unwrap();
    let own_wrap = log
        .draft_wrap(&actor, 3, epoch_id, &key, lab.identities[0].recipient())
        .unwrap()
        .sign(&[31; 32], [3; 16])
        .unwrap();
    log.insert(own_wrap).unwrap();
    let admit = log
        .draft(
            &actor,
            4,
            Body::Admit {
                entrant: entrant(1),
                evidence: Admission::Phrase,
            },
        )
        .sign(&[31; 32], [3; 16])
        .unwrap();
    let admit_id = *admit.id();
    log.insert(admit).unwrap();
    let wrapped = log
        .draft_wrap(&actor, 5, epoch_id, &key, lab.identities[1].recipient())
        .unwrap()
        .sign(&[31; 32], [3; 16])
        .unwrap();
    let wrapped_id = *wrapped.id();
    log.insert(wrapped).unwrap();
    let bare = log_witness_from_records(lab.trust, log.ancestor_chain(&admit_id).unwrap()).unwrap();
    assert!(
        bare.get(&wrapped_id).is_none(),
        "the seat ancestry omits the selected later proof"
    );
    let endpoint = Endpoint::configured_log(
        &lab.path(1),
        lab.trust,
        &lab.requests[1],
        Arc::new(log.clone()),
        &[32; 32],
    )
    .unwrap();
    let mut stream = Cursor::new(Vec::new());
    crate::control::frontier::send_evidence(
        &endpoint,
        &mut stream,
        RecordsBound::here(false),
        &crate::control::frontier::Evidence::Log(Arc::new(log.clone())),
        &lab.requests[1],
    )
    .unwrap();
    stream.set_position(0);
    let header = receive_frame(&mut stream, 22).unwrap();
    let count = u32::from_be_bytes(header[10..14].try_into().unwrap()) as usize;
    let records: Vec<_> = (0..count)
        .map(|_| {
            Record::decode(
                &receive_frame(&mut stream, aterm_secrets::membership_log::MAX_RECORD).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let received = log_witness_from_records(lab.trust, records).unwrap();
    assert_eq!(received.held_count(), log.held_count());
    assert!(received.get(&wrapped_id).is_some());
    admit_by_log_witness(&received, lab.requests[1].binding()).unwrap();
}

struct LostReceipt<S> {
    inner: S,
    failed: bool,
}

impl<S: Read> Read for LostReceipt<S> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(out)
    }
}
impl<S: Write> Write for LostReceipt<S> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.failed && bytes.starts_with(b"ACK4") {
            self.failed = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "lost durable receipt",
            ));
        }
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[test]
fn frontier_native_tls_resumes_after_a_durable_receipt_is_lost() {
    let fixture = RecordsLab::new();
    let expected = fixture.names(0, 519);
    std::fs::write(
        fixture.paths[0].records_file().with_extension("crossed"),
        b"crossed\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (tcp, _) = Connection::admit(listener.accept().unwrap().0, Scope::Loopback)
                .unwrap()
                .into_parts();
            tcp.set_read_timeout(Some(EXCHANGE)).unwrap();
            tcp.set_write_timeout(Some(EXCHANGE)).unwrap();
            let mut channel = fleet::server_handshake_database_as(
                tcp,
                &FleetSecret::new(&[51; 32]).unwrap(),
                Some(&fixture.endpoints[1].identity),
                EXCHANGE,
            )
            .unwrap();
            assert_eq!(
                channel.transport.stream().conn.alpn_protocol(),
                Some(fleet::ALPN_DATABASE_FRONTIER)
            );
            let mut cut = LostReceipt {
                inner: channel.transport.stream(),
                failed: false,
            };
            let mut repair = Repair::default();
            let report = fixture.endpoints[1].connected_until(
                &mut cut,
                channel.peer.as_ref(),
                Side::Responder,
                &mut repair,
                Instant::now() + EXCHANGE,
                Protocol::Frontier,
            );
            assert!(cut.failed);
            assert!(report.is_ok());
            assert!(repair.membership_error().is_some());
        });
        let (tcp, _) = Connection::connect(address, Scope::Loopback, EXCHANGE)
            .unwrap()
            .into_parts();
        tcp.set_read_timeout(Some(EXCHANGE)).unwrap();
        tcp.set_write_timeout(Some(EXCHANGE)).unwrap();
        let mut channel = fleet::client_handshake_database_as(
            tcp,
            &FleetSecret::new(&[51; 32]).unwrap(),
            Some(&fixture.endpoints[0].identity),
            EXCHANGE,
        )
        .unwrap();
        assert_eq!(
            channel.transport.stream().conn.alpn_protocol(),
            Some(fleet::ALPN_DATABASE_FRONTIER)
        );
        let mut repair = Repair::default();
        let report = fixture.endpoints[0].connected_until(
            channel.transport.stream(),
            channel.peer.as_ref(),
            Side::Initiator,
            &mut repair,
            Instant::now() + EXCHANGE,
            Protocol::Frontier,
        );
        assert!(report.is_ok());
        assert!(repair.membership_error().is_some());
        server.join().unwrap();
    });
    let retained = client::load_records(&fixture.paths[1])
        .unwrap()
        .inventory()
        .len();
    assert!(retained > 1 && retained < expected.held_count());
    assert!(client::records_marked_crossed(&fixture.paths[1]).unwrap());
    let (server, client_result, repair) = dial(&fixture.endpoints[1], &fixture.endpoints[0]);
    assert!(
        server.is_ok() && client_result.is_ok(),
        "{server:?} {client_result:?}"
    );
    assert!(
        repair.membership_error().is_none(),
        "{:?}",
        repair.membership_error()
    );
    assert_eq!(
        client::load_records(&fixture.paths[1]).unwrap().inventory(),
        expected.records().map(|record| *record.id()).collect()
    );
}

fn evidence_header(bound: RecordsBound, log: bool, count: u32, bytes: u64) -> Vec<u8> {
    let mut header = b"AEV4".to_vec();
    header.extend_from_slice(&bound.held.to_be_bytes());
    header.push(u8::from(bound.crossed));
    header.push(u8::from(log));
    header.extend_from_slice(&count.to_be_bytes());
    header.extend_from_slice(&bytes.to_be_bytes());
    header
}

#[test]
fn frontier_rejects_oversized_evidence_before_reading_its_records() {
    let lab = Lab::new();
    let endpoint = lab.endpoint(1);
    let remote = lab.endpoint(0);
    let bound = RecordsBound::here(false);
    let metadata = Metadata {
        request: lab.requests[0].clone(),
        bound,
        flags: 0,
    };
    let proof = NodeProof {
        node: remote.identity.node().into(),
        key: remote.identity.key(),
    };
    for (log, count, bytes) in [
        (true, 0, 0),
        (true, bound.held + 1, 1),
        (
            true,
            1,
            aterm_secrets::membership_log::MAX_RECORDS_STREAM as u64 + 1,
        ),
        (false, 2, 1),
        (false, 1, MAX_NODE as u64 + 1),
    ] {
        let mut input = Vec::new();
        send_frame(&mut input, &metadata.encode(lab.trust)).unwrap();
        send_frame(&mut input, &evidence_header(bound, log, count, bytes)).unwrap();
        let before_records = input.len() as u64;
        send_frame(&mut input, b"unread record").unwrap();
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let mut repair = Repair::default();
        assert!(
            endpoint
                .connected_until(
                    &mut stream,
                    Some(&proof),
                    Side::Responder,
                    &mut repair,
                    Instant::now() + EXCHANGE,
                    Protocol::Frontier
                )
                .is_err()
        );
        assert_eq!(stream.input.position(), before_records);
        assert_eq!(stream.output.len(), 4 + HELLO_FIXED + 6);
        assert!(repair.authenticated_actor().is_none());
    }
}

#[test]
fn frontier_rejects_a_different_real_genesis_even_when_root_metadata_matches() {
    let fixture = RecordsLab::new();
    let remote = &fixture.endpoints[0];
    let foreign = Record::found(
        &ROOT,
        fixture
            .lab
            .requests
            .iter()
            .enumerate()
            .map(|(n, request)| Entrant {
                request: request.clone(),
                node_key: *fixture.endpoints[n].identity.key().bytes(),
                name: format!("m{}", n + 1),
                label: format!("m{}", n + 1),
            })
            .collect(),
        99,
    )
    .unwrap();
    assert_ne!(foreign.id(), fixture.log.network());
    let bound = RecordsBound::here(false);
    let metadata = Metadata {
        request: fixture.lab.requests[0].clone(),
        bound,
        flags: 1,
    };
    let mut input = Vec::new();
    send_frame(&mut input, &metadata.encode(fixture.lab.trust)).unwrap();
    send_frame(
        &mut input,
        &evidence_header(bound, true, 1, foreign.bytes().len() as u64),
    )
    .unwrap();
    send_frame(&mut input, foreign.bytes()).unwrap();
    let mut stream = Script {
        input: Cursor::new(input),
        output: Vec::new(),
    };
    let proof = NodeProof {
        node: remote.identity.node().into(),
        key: remote.identity.key(),
    };
    let mut repair = Repair::default();
    assert!(
        fixture.endpoints[1]
            .connected_until(
                &mut stream,
                Some(&proof),
                Side::Responder,
                &mut repair,
                Instant::now() + EXCHANGE,
                Protocol::Frontier
            )
            .is_err()
    );
    assert_eq!(stream.output.len(), 4 + HELLO_FIXED + 6);
    assert!(repair.authenticated_actor().is_none());
}

#[test]
fn frontier_rechecks_capacity_after_waiting_for_peer_admission() {
    for cross_during_admission in [false, true] {
        let lab = Lab::new();
        let paths = client::Paths::for_home(&lab.root.path().join("late-mark"));
        std::fs::create_dir_all(&paths.dir).unwrap();
        let mark = paths.records_file().with_extension("crossed");
        let mut endpoint = lab.endpoint(1).with_membership_paths(paths);
        endpoint.records_bound = RecordsBound::of(8192, false);
        let mut remote = lab.endpoint(0);
        remote.records_bound = RecordsBound::of(4096, false);
        let metadata = Metadata {
            request: lab.requests[0].clone(),
            bound: remote.records_bound,
            flags: 0,
        };
        let mut input = Vec::new();
        send_frame(&mut input, &metadata.encode(lab.trust)).unwrap();
        let mut evidence = Cursor::new(Vec::new());
        crate::control::frontier::send_evidence(
            &remote,
            &mut evidence,
            endpoint.records_bound,
            &crate::control::frontier::Evidence::Node(remote.hello[HELLO_FIXED..].to_vec()),
            &lab.requests[0],
        )
        .unwrap();
        input.extend_from_slice(&evidence.into_inner());
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let proof = NodeProof {
            node: remote.identity.node().into(),
            key: remote.identity.key(),
        };
        let mut repair = Repair::default();
        let _ = observe_peer_cache(
            move |stage| {
                if cross_during_admission
                    && matches!(stage, PeerCacheStage::Acquired(Side::Responder))
                {
                    std::fs::write(&mark, b"crossed\n").unwrap();
                }
            },
            || {
                endpoint.connected_until(
                    &mut stream,
                    Some(&proof),
                    Side::Responder,
                    &mut repair,
                    Instant::now() + EXCHANGE,
                    Protocol::Frontier,
                )
            },
        );
        assert_eq!(
            stream.output.len() == 4 + HELLO_FIXED + 6,
            cross_during_admission
        );
    }
}

#[test]
fn frontier_native_tls_repairs_history_past_the_legacy_count() {
    use aterm_secrets::membership_log::{LEGACY_RECORDS, RecordBudgets};
    const LARGER: RecordBudgets = RecordBudgets {
        held: 8192,
        ..RecordBudgets::PRODUCTION
    };
    LARGER.within(|| {
        let mut fixture = RecordsLab::new();
        let expected = fixture.names(0, LEGACY_RECORDS as u64 + 2);
        assert!(client::records_marked_crossed(&fixture.paths[0]).unwrap());
        assert!(
            aterm_secrets::membership_log::decode_records(&encode_records(expected.records()))
                .is_err()
        );
        fixture.endpoints[0] = Endpoint::configured_log(
            &fixture.lab.path(0),
            fixture.lab.trust,
            &fixture.lab.requests[0],
            Arc::new(expected.clone()),
            &[31; 32],
        )
        .expect("a local authority witness may exceed the legacy wire count")
        .with_membership_paths(fixture.paths[0].clone());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut repair = Repair::default();
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                LARGER.within(|| {
                    fixture.endpoints[1].exchange(
                        Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                        Side::Responder,
                        &mut Repair::default(),
                    )
                })
            });
            let client_result = fixture.endpoints[0].exchange(
                Connection::connect(address, Scope::Loopback, EXCHANGE).unwrap(),
                Side::Initiator,
                &mut repair,
            );
            let server = server.join().unwrap();
            assert!(
                server.is_ok() && client_result.is_ok(),
                "{server:?} {client_result:?}"
            );
        });
        assert!(
            repair.membership_error().is_none(),
            "{:?}",
            repair.membership_error()
        );
        let retained = client::load_records(&fixture.paths[1]).unwrap();
        assert_eq!(retained.inventory().len(), LEGACY_RECORDS + 3);
        assert_eq!(
            retained.inventory(),
            expected.records().map(|record| *record.id()).collect()
        );
        assert!(client::records_marked_crossed(&fixture.paths[1]).unwrap());
    });
}
