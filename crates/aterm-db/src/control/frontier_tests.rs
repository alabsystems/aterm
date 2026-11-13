// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::frontier::{
    BATCH_BYTES, BATCH_RECORDS, Evidence, MAX_EVIDENCE, Metadata, Session, View, WHOLE_BATCH_BYTES,
    WHOLE_BATCH_RECORDS, Wire, encode_frontier,
};
use aterm_secrets::{
    client,
    crossing::RecordsBound,
    membership_log::{Body, Draft, Entrant, Log, Record, RecordBudgets, encode_records},
};
use std::net::TcpStream;

/// The held total of the two budgets together, past the legacy count: what the tests of logs
/// above 4,096 records run under ([`RecordBudgets::within`]), as the activation raises it.
const RAISED: RecordBudgets = RecordBudgets {
    held: aterm_secrets::membership_log::MAX_WEIGHED_RECORDS
        + aterm_secrets::membership_log::MAX_KEYED_RECORDS,
    ..RecordBudgets::PRODUCTION
};

/// The channel proof `endpoint` presents.
fn proof_of(endpoint: &Endpoint) -> NodeProof {
    NodeProof {
        node: endpoint.identity.node().into(),
        key: endpoint.identity.key(),
    }
}

/// The bytes `endpoint` writes before any evidence under `wire`: its metadata, then under
/// `atdb/5` what it holds.
fn stated(endpoint: &Endpoint, wire: Wire) -> usize {
    let metadata = 4 + HELLO_FIXED + 6;
    match wire {
        Wire::Whole => metadata,
        Wire::Delta => metadata + 4 + 13 + 32 * View::of(endpoint).unwrap().have().len(),
    }
}

/// Metadata of `request` under `bound`, then under `atdb/5` a statement that it holds nothing:
/// what a peer sends before its evidence.
fn opening(trust: Trust, request: &Request, bound: RecordsBound, wire: Wire) -> Vec<u8> {
    let mut input = Vec::new();
    let metadata = Metadata {
        request: request.clone(),
        bound,
        flags: 0,
    };
    send_frame(&mut input, &metadata.encode(trust)).unwrap();
    if wire == Wire::Delta {
        send_frame(&mut input, &encode_frontier(bound, &[])).unwrap();
    }
    input
}

/// The frames `endpoint` writes pushing `evidence` for `target` under `wire` to a peer that
/// states `remote` and that it holds `have`, from their start.
fn evidence_frames(
    endpoint: &Endpoint,
    remote: RecordsBound,
    evidence: &Evidence,
    target: &Request,
    wire: Wire,
    have: &[Digest],
) -> Cursor<Vec<u8>> {
    let mut stream = Cursor::new(Vec::new());
    let none = View::NONE;
    crate::control::frontier::send_evidence(
        endpoint,
        &mut stream,
        remote,
        evidence,
        target,
        &Session {
            wire,
            view: &none,
            have,
        },
    )
    .unwrap();
    stream.set_position(0);
    stream
}

/// `count` `Name` records by Lab Mac `member`, each following the one before, the first following
/// `after` and `previous` (`member`'s record before them, and its sequence): a chain drafted in
/// constant time per record, where [`Log::draft`] walks the log.
fn chain(
    network: Digest,
    member: usize,
    after: Digest,
    previous: Option<(Digest, u64)>,
    count: usize,
) -> Vec<Record> {
    let (mut previous, mut sequence) = previous.map_or((None, 0), |(id, n)| (Some(id), n));
    let mut parent = after;
    (0..count)
        .map(|_| {
            sequence += 1;
            let mut parents = vec![parent];
            parents.extend(previous.filter(|id| *id != parent));
            let record = Draft {
                network,
                sequence,
                previous,
                parents,
                authored_ms: 1_000 + sequence,
                body: Body::Name {
                    name: format!("chain-{member}-{sequence}"),
                },
            }
            .sign(&[member as u8 + 31; 32], [member as u8 + 3; 16])
            .unwrap();
            previous = Some(*record.id());
            parent = *record.id();
            record
        })
        .collect()
}

/// A log of `records`, the first its genesis.
fn log_of(records: &[Record]) -> Log {
    let mut log = Log::new(records[0].clone()).unwrap();
    for record in &records[1..] {
        log.insert(record.clone()).unwrap();
    }
    log
}

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
    let mut extended = wire.to_vec();
    extended.push(0);
    assert!(Metadata::decode(lab.trust, &extended).is_err());
    for at in [0, 4, 20, 52, HELLO_FIXED + 4, HELLO_FIXED + 5] {
        let mut damaged = wire;
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
    for (wire, proof, bound) in [Wire::Whole, Wire::Delta].into_iter().flat_map(|wire| {
        [
            (wire, None, RecordsBound::here(false)),
            (wire, Some(&reflected), RecordsBound::here(false)),
            (
                wire,
                Some(&valid_proof),
                RecordsBound::of(u32::MAX as usize, true),
            ),
        ]
    }) {
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
                    Protocol::Frontier(wire)
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
        keep(&self.paths[member], &log);
        log
    }
}

/// Make `log` the records `paths` holds: crossed, as custody past the legacy count is.
fn keep(paths: &client::Paths, log: &Log) {
    let wire = encode_records(log.records());
    if log.held_count() > aterm_secrets::membership_log::LEGACY_RECORDS {
        // Construct future-budget custody without relaxing the public legacy wire decoder.
        aterm_secrets::fsx::write_atomic(
            &paths.records_file().with_extension("crossed"),
            b"crossed\n",
            0o600,
        )
        .unwrap();
        aterm_secrets::fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    } else {
        client::save_records(paths, &wire).unwrap();
    }
}

/// Records custody of Lab Mac `member` (its label `m<member + 1>`) under `root`, its vault pinned
/// to [`ROOT`]'s key, holding `log`.
fn custody(root: &Path, member: usize, log: &Log) -> client::Paths {
    let paths = client::Paths::for_home(root);
    let pin = aterm_codec::base64::encode(&aterm_secrets::envelope::signer_public(&ROOT).unwrap())
        .unwrap();
    client::configure_log_member(
        &paths,
        &format!("{}#{pin}", root.join("team.vault").display()),
        &format!("m{}", member + 1),
    )
    .unwrap();
    keep(&paths, log);
    paths
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
                            Connection::admit(
                                aterm_uds::accept(
                                    &listener,
                                    aterm_uds::AcceptWait::Forever,
                                    aterm_uds::AcceptedMode::Blocking,
                                )
                                .unwrap()
                                .0,
                                Scope::Loopback,
                            )
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
            // Left for a later session, the records hold up nothing that follows them.
            assert!(repair.membership_error().is_none(), "{repair:?}");
            assert!(
                repair
                    .membership()
                    .is_some_and(|report| report.deferred > 0)
            );
            assert_eq!(repair.phase(), Phase::ControlComplete);
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

/// An `atdb/5` membership round's batch holds at most [`BATCH_RECORDS`] records and
/// [`BATCH_BYTES`] bytes, but for one record larger than that, which goes alone; batches rotate
/// through every record. Such a session runs at most [`Wire::round_cap`] rounds, counted from the
/// records both sides hold, not one for every three records. An `atdb/4` batch keeps 0.153's
/// bounds, [`WHOLE_BATCH_RECORDS`] records and [`WHOLE_BATCH_BYTES`] bytes, and its count of
/// rounds, which a 0.153 peer shares.
///
/// NEGATIVE CONTROL (in the test): the log holds batches each bound stops (more records than one
/// batch takes, values that fill a batch's bytes, and a record past them), and each is seen to.
#[test]
fn frontier_batches_visit_every_record_with_count_and_byte_bounds() {
    use aterm_secrets::{
        SecretBytes,
        membership_log::EpochAudience,
        text::{Audience, Entry, MAX_VALUE},
    };
    let fixture = RecordsLab::new();
    let mut log = fixture.names(0, BATCH_RECORDS as u64 + 76);
    let actor = fixture.lab.requests[0].binding().actor();
    let (draft, key) = log
        .draft_epoch(&actor, 1_200, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&[31; 32], [3; 16]).unwrap();
    let epoch_id = *epoch.id();
    log.insert(epoch).unwrap();
    for (n, size) in [384 * 1024, 384 * 1024, 384 * 1024, MAX_VALUE, 384 * 1024]
        .into_iter()
        .enumerate()
    {
        let entry = Entry {
            name: format!("batch-payload-{n}"),
            value: SecretBytes::new(vec![0x53; size]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let record = log
            .draft_put(&actor, 1_201 + n as u64, epoch_id, &key, &entry)
            .unwrap()
            .sign(&[31; 32], [3; 16])
            .unwrap();
        log.insert(record).unwrap();
    }
    let records: Vec<_> = log.records().cloned().collect();
    let expected: BTreeSet<_> = records.iter().map(|record| *record.id()).collect();
    let mut seen = BTreeSet::new();
    let mut cursor = None;
    let (mut count_limited, mut byte_limited, mut alone) = (false, false, false);
    for _ in 0..records.len() {
        let batch = crate::control::frontier::select_batch(Wire::Delta, &records, &mut cursor);
        assert!(!batch.is_empty());
        assert!(batch.len() <= BATCH_RECORDS);
        let bytes: usize = batch.iter().map(|record| record.bytes().len()).sum();
        if batch.len() == 1 && bytes > BATCH_BYTES {
            alone = true;
        } else {
            assert!(bytes <= BATCH_BYTES);
        }
        count_limited |= batch.len() == BATCH_RECORDS;
        byte_limited |= batch.len() > 1 && batch.len() < BATCH_RECORDS && bytes > BATCH_BYTES / 2;
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
    assert!(count_limited && byte_limited && alone);
    // Two rounds move a full batch of records or bytes: 138 rounds at 4,096 records and 162 at
    // 16,384, where one round per three records came to 1,367 and 5,463.
    assert_eq!(Wire::Delta.round_cap(4_096), 138);
    assert_eq!(Wire::Delta.round_cap(16_384), 162);

    // `atdb/4` batches stay within what a 0.153 peer takes, and visit every record too.
    let (mut seen, mut cursor, mut count_limited) = (BTreeSet::new(), None, false);
    while seen != expected {
        let batch = crate::control::frontier::select_batch(Wire::Whole, &records, &mut cursor);
        let bytes: usize = batch.iter().map(|record| record.bytes().len()).sum();
        assert!(!batch.is_empty() && batch.len() <= WHOLE_BATCH_RECORDS);
        assert!(bytes <= WHOLE_BATCH_BYTES);
        count_limited |= batch.len() == WHOLE_BATCH_RECORDS;
        let before = seen.len();
        seen.extend(batch.iter().map(|record| *record.id()));
        assert!(seen.len() > before);
    }
    assert!(count_limited);
    // 0.153's count, which both sides of an `atdb/4` session must share.
    assert_eq!(
        (WHOLE_BATCH_RECORDS, WHOLE_BATCH_BYTES),
        (256, 4 * 1024 * 1024)
    );
    assert_eq!(Wire::Whole.round_cap(4_096), 1_367);
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
    let pushed = |wire: Wire, have: &[Digest]| {
        let mut stream = evidence_frames(
            &endpoint,
            RecordsBound::here(false),
            &Evidence::Log(Arc::new(log.clone())),
            &lab.requests[1],
            wire,
            have,
        );
        let header = receive_frame(&mut stream, 22).unwrap();
        let count = u32::from_be_bytes(header[10..14].try_into().unwrap()) as usize;
        (0..count)
            .map(|_| {
                Record::decode(
                    &receive_frame(&mut stream, aterm_secrets::membership_log::MAX_RECORD).unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>()
    };
    // Under `atdb/4`, and to a peer that holds nothing, the whole selected evidence, the later
    // proof among it.
    for wire in [Wire::Whole, Wire::Delta] {
        let received = log_witness_from_records(lab.trust, pushed(wire, &[])).unwrap();
        assert_eq!(received.held_count(), log.held_count());
        assert!(received.get(&wrapped_id).is_some());
        admit_by_log_witness(&received, lab.requests[1].binding()).unwrap();
    }
    // To a peer that holds the seat's ancestry, the later proof alone; to one that holds it too,
    // nothing.
    let later = pushed(Wire::Delta, &[admit_id]);
    assert_eq!(
        later.iter().map(|record| *record.id()).collect::<Vec<_>>(),
        vec![wrapped_id]
    );
    assert!(pushed(Wire::Delta, &[wrapped_id]).is_empty());
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
    // More than one batch, so the lost receipt leaves part of the log retained.
    let expected = fixture.names(0, BATCH_RECORDS as u64 + 7);
    std::fs::write(
        fixture.paths[0].records_file().with_extension("crossed"),
        b"crossed\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (tcp, _) = Connection::admit(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
                Scope::Loopback,
            )
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
                Some(fleet::ALPN_DATABASE_DELTA)
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
                Protocol::Frontier(Wire::Delta),
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
            Some(fleet::ALPN_DATABASE_DELTA)
        );
        let mut repair = Repair::default();
        let report = fixture.endpoints[0].connected_until(
            channel.transport.stream(),
            channel.peer.as_ref(),
            Side::Initiator,
            &mut repair,
            Instant::now() + EXCHANGE,
            Protocol::Frontier(Wire::Delta),
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

/// A valid typed witness can exceed an unmarked peer's smaller held capacity even when the
/// two advertised capacities can exchange. Reject that push before sending any bytes; the
/// same evidence fits a peer whose held bound equals its count.
#[test]
fn frontier_sender_respects_the_receivers_smaller_held_bound() {
    RAISED.within(|| {
        let fixture = RecordsLab::new();
        let log = fixture.names(0, aterm_secrets::membership_log::LEGACY_RECORDS as u64);
        let count = log.held_count();
        assert!(count > aterm_secrets::membership_log::LEGACY_RECORDS);
        let endpoint = fixture.lab.endpoint(0);
        let evidence = crate::control::frontier::Evidence::Log(Arc::new(log));
        for (wire, held, fits) in [Wire::Whole, Wire::Delta]
            .into_iter()
            .flat_map(|wire| [(wire, count, true), (wire, count - 1, false)])
        {
            let remote = RecordsBound::of(held, false);
            assert!(endpoint.records_bound.can_exchange(remote));
            let mut output = Cursor::new(Vec::new());
            let sent = crate::control::frontier::send_evidence(
                &endpoint,
                &mut output,
                remote,
                &evidence,
                &fixture.lab.requests[0],
                &Session {
                    wire,
                    view: &View::NONE,
                    have: &[],
                },
            );
            assert_eq!(sent.is_ok(), fits, "held={held}, count={count}");
            assert_eq!(output.get_ref().is_empty(), !fits);
        }
    });
}

/// Evidence past its bounds is refused from its header, before a record of it is read: more
/// records than either side holds, more than the wire's evidence bytes (under `atdb/5`
/// [`MAX_EVIDENCE`], what an `ACX2` hello carried of a witness before it; under `atdb/4`
/// `MAX_RECORDS_STREAM`, 64 MiB, as 0.153 takes), a journal node that is not one, or larger than
/// one; and an empty push, which `atdb/4` refuses from its header and which seats nobody in a
/// log that holds nothing under `atdb/5`.
///
/// NEGATIVE CONTROL (in the test): a push of exactly the wire's evidence bytes passes its header,
/// and its first record is read.
#[test]
fn frontier_rejects_oversized_evidence_before_reading_its_records() {
    use aterm_secrets::membership_log::MAX_RECORDS_STREAM;
    let lab = Lab::new();
    let endpoint = lab.endpoint(1);
    let remote = lab.endpoint(0);
    let bound = RecordsBound::here(false);
    let proof = proof_of(&remote);
    for (wire, log, count, bytes, refused) in [
        (Wire::Delta, true, 0, 0, true),
        (Wire::Delta, true, bound.held + 1, 1, true),
        (Wire::Delta, true, 1, MAX_EVIDENCE as u64 + 1, true),
        (Wire::Delta, true, 1, MAX_EVIDENCE as u64, false),
        (Wire::Delta, false, 0, 0, true),
        (Wire::Delta, false, 2, 1, true),
        (Wire::Delta, false, 1, MAX_NODE as u64 + 1, true),
        (Wire::Whole, true, 0, 0, true),
        (Wire::Whole, true, bound.held + 1, 1, true),
        (Wire::Whole, true, 1, MAX_RECORDS_STREAM as u64 + 1, true),
        (Wire::Whole, true, 1, MAX_RECORDS_STREAM as u64, false),
        (Wire::Whole, false, 0, 0, true),
        (Wire::Whole, false, 2, 1, true),
        (Wire::Whole, false, 1, MAX_NODE as u64 + 1, true),
    ] {
        let mut input = opening(lab.trust, &lab.requests[0], bound, wire);
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
                    Protocol::Frontier(wire)
                )
                .is_err()
        );
        assert_eq!(
            stream.input.position() == before_records,
            refused,
            "{wire:?} {log} {count} {bytes}"
        );
        assert_eq!(stream.output.len(), stated(&endpoint, wire));
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
    let held = client::load_records(&fixture.paths[1]).unwrap().inventory();
    for wire in [Wire::Whole, Wire::Delta] {
        let mut input = Vec::new();
        send_frame(&mut input, &metadata.encode(fixture.lab.trust)).unwrap();
        if wire == Wire::Delta {
            send_frame(&mut input, &encode_frontier(bound, &[])).unwrap();
        }
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
                    Protocol::Frontier(wire)
                )
                .is_err()
        );
        assert_eq!(stream.output.len(), stated(&fixture.endpoints[1], wire));
        assert!(repair.authenticated_actor().is_none());
        assert_eq!(
            client::load_records(&fixture.paths[1]).unwrap().inventory(),
            held,
            "a refused push leaves nothing behind"
        );
    }
}

#[test]
fn frontier_rechecks_capacity_after_waiting_for_peer_admission() {
    for (wire, cross_during_admission) in [
        (Wire::Whole, false),
        (Wire::Whole, true),
        (Wire::Delta, false),
        (Wire::Delta, true),
    ] {
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
        if wire == Wire::Delta {
            send_frame(&mut input, &encode_frontier(remote.records_bound, &[])).unwrap();
        }
        let evidence = evidence_frames(
            &remote,
            endpoint.records_bound,
            &Evidence::Node(remote.hello[HELLO_FIXED..].to_vec()),
            &lab.requests[0],
            wire,
            &[],
        );
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
        let before_evidence = stated(&endpoint, wire);
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
                    Protocol::Frontier(wire),
                )
            },
        );
        assert_eq!(
            stream.output.len() == before_evidence,
            cross_during_admission
        );
    }
}

#[test]
fn frontier_native_tls_repairs_history_past_the_legacy_count() {
    use aterm_secrets::membership_log::LEGACY_RECORDS;
    let mut fixture = RecordsLab::new();
    let expected = fixture.names(0, LEGACY_RECORDS as u64 + 2);
    assert!(client::records_marked_crossed(&fixture.paths[0]).unwrap());
    assert_eq!(
        aterm_secrets::membership_log::decode_records(&encode_records(expected.records()))
            .unwrap()
            .len(),
        expected.held_count()
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
            fixture.endpoints[1].exchange(
                Connection::admit(
                    aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    )
                    .unwrap()
                    .0,
                    Scope::Loopback,
                )
                .unwrap(),
                Side::Responder,
                &mut Repair::default(),
            )
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
}

/// A membership-log witness is what a peer on `atdb/3` or older sends (an `ACX2` hello, an `ACW2`
/// enrollment answer), so it is decoded within that build's bound whatever this build holds: a
/// witness of [`aterm_secrets::membership_log::LEGACY_RECORDS`] records decodes, one more is
/// refused. Built under a raised held total, where `atdb/5` evidence of the same log is taken
/// (`frontier_native_tls_repairs_history_past_the_legacy_count`).
///
/// NEGATIVE CONTROL (in the test): a decoder bounded by the held budget
/// ([`mutants::Mutant::WitnessAtHeldBudget`]) takes the longer witness. The decoder before this
/// bound read `MAX_HELD_RECORDS`, a constant still equal to the legacy count, so it refused this
/// witness too: the mutant stands for what it becomes once the activation raises the held total.
/// The same witness one record shorter decoding is a positive control.
#[test]
fn a_legacy_witness_past_the_legacy_count_is_refused() {
    use aterm_secrets::membership_log::LEGACY_RECORDS;
    RecordBudgets {
        held: LEGACY_RECORDS + 1,
        ..RecordBudgets::PRODUCTION
    }
    .within(|| {
        let fixture = RecordsLab::new();
        let log = fixture.names(0, LEGACY_RECORDS as u64);
        let records: Vec<_> = log.records().cloned().collect();
        assert_eq!(records.len(), LEGACY_RECORDS + 1);
        let witness = |n: usize| encode_records(&records[..n]);
        let decoded = decode_log_witness(fixture.lab.trust, &witness(LEGACY_RECORDS))
            .expect("a legacy peer's whole witness");
        assert_eq!(decoded.held_count(), LEGACY_RECORDS);
        assert!(decode_log_witness(fixture.lab.trust, &witness(LEGACY_RECORDS + 1)).is_err());
        let mutant = mutants::with(mutants::Mutant::WitnessAtHeldBudget, || {
            decode_log_witness(fixture.lab.trust, &witness(LEGACY_RECORDS + 1))
        });
        assert_eq!(
            mutant.expect("the held budget takes it").held_count(),
            LEGACY_RECORDS + 1
        );
    });
}

/// Lab Mac `n` as a log's entrant.
fn entrant(lab: &Lab, n: usize) -> Entrant {
    Entrant {
        request: lab.requests[n].clone(),
        node_key: *lab.endpoint(n).identity.key().bytes(),
        name: format!("m{}", n + 1),
        label: format!("m{}", n + 1),
    }
}

/// Lab Mac `member` with `log` as its seat evidence, keeping no records custody.
fn witnessed(lab: &Lab, member: usize, log: Log) -> Endpoint {
    Endpoint::configured_log(
        &lab.path(member),
        lab.trust,
        &lab.requests[member],
        Arc::new(log),
        &[member as u8 + 31; 32],
    )
    .unwrap()
}

/// Lab Mac `member` holding `held` in records custody of its own, named `name`, its seat evidence
/// `witness`.
fn custodied(lab: &Lab, member: usize, name: &str, witness: Log, held: &Log) -> Endpoint {
    let home = lab.root.path().join(name);
    aterm_secrets::fsx::ensure_private_dir(&home.join("journal")).unwrap();
    drop(Journal::open(&home.join("journal"), lab.trust).unwrap());
    let mut endpoint = Endpoint::configured_log(
        &home.join("journal"),
        lab.trust,
        &lab.requests[member],
        Arc::new(witness),
        &[member as u8 + 31; 32],
    )
    .unwrap();
    endpoint.paths = Some(custody(&home, member, held));
    endpoint
}

/// One end of a socket that counts the bytes it writes.
struct Counted {
    stream: TcpStream,
    written: usize,
}

impl Read for Counted {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(out)
    }
}

impl Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.stream.write(bytes)?;
        self.written += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

/// One `atdb/5` session from `initiator` to `responder` over loopback, each side under `budgets`:
/// what each side returned and its repair (the initiator's first), and the bytes both wrote.
fn counted_session(
    initiator: &Endpoint,
    responder: &Endpoint,
    budgets: RecordBudgets,
) -> ([(Result<Report>, Repair); 2], usize) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let run = |endpoint: &Endpoint, peer: &Endpoint, stream: TcpStream, side: Side| {
        budgets.within(|| {
            stream
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            let mut stream = Counted { stream, written: 0 };
            let mut repair = Repair::default();
            let result = endpoint.connected_until(
                &mut stream,
                Some(&proof_of(peer)),
                side,
                &mut repair,
                Instant::now() + EXCHANGE,
                Protocol::Frontier(Wire::Delta),
            );
            ((result, repair), stream.written)
        })
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            run(
                responder,
                initiator,
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
                Side::Responder,
            )
        });
        let dialed = run(
            initiator,
            responder,
            TcpStream::connect(address).unwrap(),
            Side::Initiator,
        );
        let answered = answered.join().unwrap();
        ([dialed.0, answered.0], dialed.1 + answered.1)
    })
}

/// How many records `endpoint`'s `atdb/5` evidence push to `receiver` carries: its seat evidence
/// `witness` for Lab Mac `member`, less the past of what `receiver` states it holds.
fn pushed_to(
    lab: &Lab,
    member: usize,
    endpoint: &Endpoint,
    witness: &Log,
    receiver: &Endpoint,
) -> usize {
    let mut stream = evidence_frames(
        endpoint,
        RecordsBound::here(true),
        &Evidence::Log(Arc::new(witness.clone())),
        &lab.requests[member],
        Wire::Delta,
        &View::of(receiver).unwrap().have(),
    );
    let header = receive_frame(&mut stream, 22).unwrap();
    u32::from_be_bytes(header[10..14].try_into().unwrap()) as usize
}

/// A Mac admitted at a log's 10,000th record authenticates over `atdb/5` to a peer that lacks its
/// `Admit`, pushing that record alone (its seat evidence, the whole log, less the past of what the
/// peer stated it holds), and to a peer that holds it, pushing nothing. Each peer judges the push
/// in a copy of its own records. Its logs pass 4,096 records, so the test runs under a raised held
/// total ([`RAISED`], [`RecordBudgets::within`]), as the activation raises it.
///
/// NEGATIVE CONTROL (in the test): the peer that lacks the `Admit`, sent an empty push, refuses
/// the Mac; the push the evidence before this fix sent is the whole witness, 10,000 records.
#[test]
fn a_mac_admitted_at_record_ten_thousand_pushes_only_what_its_peer_lacks() {
    RAISED.within(|| {
        let lab = Lab::new();
        let genesis = Record::found(&ROOT, vec![entrant(&lab, 0)], 1).unwrap();
        let network = *genesis.id();
        let mut records = vec![genesis.clone()];
        records.extend(chain(network, 0, network, None, 9_998));
        let last = *records.last().unwrap().id();
        records.push(
            Draft {
                network,
                sequence: 9_999,
                previous: Some(last),
                parents: vec![last],
                authored_ms: 20_000,
                body: Body::Admit {
                    entrant: entrant(&lab, 1),
                    evidence: aterm_secrets::membership_log::Evidence::Phrase,
                },
            }
            .sign(&[31; 32], [3; 16])
            .unwrap(),
        );
        let log = log_of(&records);
        assert_eq!(log.held_count(), 10_000);
        let late = witnessed(&lab, 1, log.clone());
        let founder = |name: &str, held: &[Record]| {
            custodied(&lab, 0, name, log_of(&records[..1]), &log_of(held))
        };
        let (lacking, holding) = (
            founder("lacking", &records[..9_999]),
            founder("holding", &records),
        );

        assert_eq!(pushed_to(&lab, 1, &late, &log, &lacking), 1);
        assert_eq!(pushed_to(&lab, 1, &late, &log, &holding), 0);
        assert_eq!(pushed_to(&lab, 1, &late, &log, &lab.endpoint(0)), 10_000);
        for receiver in [&lacking, &holding] {
            let ([(dialed, dialing), (answered, answering)], _) =
                counted_session(&late, receiver, RAISED);
            assert!(
                dialed.is_ok() && answered.is_ok(),
                "{dialed:?} {answered:?}"
            );
            assert_eq!(
                answering.authenticated_actor(),
                Some(lab.requests[1].binding().actor())
            );
            assert_eq!(
                dialing.authenticated_actor(),
                Some(lab.requests[0].binding().actor())
            );
        }

        // Control: without the push, the peer that lacks the Admit holds no seat for the Mac.
        let bound = RecordsBound::here(true);
        let mut input = opening(lab.trust, &lab.requests[1], bound, Wire::Delta);
        send_frame(&mut input, &evidence_header(bound, true, 0, 0)).unwrap();
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let mut repair = Repair::default();
        assert!(
            lacking
                .connected_until(
                    &mut stream,
                    Some(&proof_of(&late)),
                    Side::Responder,
                    &mut repair,
                    Instant::now() + EXCHANGE,
                    Protocol::Frontier(Wire::Delta),
                )
                .is_err()
        );
        assert!(repair.authenticated_actor().is_none());
    });
}

/// A push reads its records custody only for the records its witness's past of what the peer
/// states it holds leaves: a Mac whose witness ends at its `Admit` pushes nothing to a peer that
/// states the `Admit`, reading no custody; the `Admit` alone to one that states the record before
/// it; and nothing to one that states a record past the witness, which only the custody holds,
/// where without a custody it pushes the whole witness.
///
/// NEGATIVE CONTROLS (round 15's controls): a push that reads its custody before its witness's
/// past fails `a push its witness settles`; one whose custody's past is never applied pushes the
/// whole witness to the peer past it (`the custody's past settles the rest`).
#[test]
fn a_push_reads_its_custody_only_for_what_its_witness_leaves() {
    use crate::control::frontier::CUSTODY_READS;
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0)], 1).unwrap();
    let network = *genesis.id();
    let mut records = vec![genesis];
    records.extend(chain(network, 0, network, None, 8));
    let last = *records.last().unwrap().id();
    let admit = Draft {
        network,
        sequence: 9,
        previous: Some(last),
        parents: vec![last],
        authored_ms: 20_000,
        body: Body::Admit {
            entrant: entrant(&lab, 1),
            evidence: aterm_secrets::membership_log::Evidence::Phrase,
        },
    }
    .sign(&[31; 32], [3; 16])
    .unwrap();
    let admitted = *admit.id();
    records.push(admit);
    let witness = log_of(&records);
    let mut beyond = records.clone();
    beyond.extend(chain(network, 0, admitted, Some((admitted, 9)), 1));
    let past = *beyond.last().unwrap().id();
    let sender = custodied(&lab, 1, "sender", witness.clone(), &log_of(&beyond));
    let push = |endpoint: &Endpoint, have: &[Digest]| {
        CUSTODY_READS.set(0);
        let mut stream = evidence_frames(
            endpoint,
            RecordsBound::here(true),
            &Evidence::Log(Arc::new(witness.clone())),
            &lab.requests[1],
            Wire::Delta,
            have,
        );
        let header = receive_frame(&mut stream, 22).unwrap();
        let pushed = u32::from_be_bytes(header[10..14].try_into().unwrap()) as usize;
        (pushed, CUSTODY_READS.get())
    };

    assert_eq!(
        push(&sender, &[admitted]),
        (0, 0),
        "a push its witness settles"
    );
    assert_eq!(push(&sender, &[last]), (1, 1), "the Admit the peer lacks");
    assert_eq!(
        push(&sender, &[past]),
        (0, 1),
        "the custody's past settles the rest"
    );
    assert_eq!(
        push(&witnessed(&lab, 1, witness.clone()), &[past]),
        (records.len(), 0),
        "without a custody, the whole witness"
    );
}

/// `record`, pushed as Lab Mac 2's whole evidence to `receiver`, answering its session: the
/// receiver's repair.
fn push_one(lab: &Lab, receiver: &Endpoint, record: &Record) -> Repair {
    let bound = RecordsBound::here(false);
    let mut input = opening(lab.trust, &lab.requests[1], bound, Wire::Delta);
    send_frame(
        &mut input,
        &evidence_header(bound, true, 1, record.bytes().len() as u64),
    )
    .unwrap();
    send_frame(&mut input, record.bytes()).unwrap();
    let mut stream = Script {
        input: Cursor::new(input),
        output: Vec::new(),
    };
    let mut repair = Repair::default();
    let _ = receiver.connected_until(
        &mut stream,
        Some(&proof_of(&lab.endpoint(1))),
        Side::Responder,
        &mut repair,
        Instant::now() + EXCHANGE,
        Protocol::Frontier(Wire::Delta),
    );
    repair
}

/// A push holding a record whose signer has no seat in its causal past is refused in the overlay
/// of the receiver's records, and its peer is not authenticated: an `Admit` of Mac 2 that a key no
/// founder or `Admit` seats signed. Nothing of it is kept.
///
/// NEGATIVE CONTROL (in the test): the same `Admit` signed by the founder authenticates Mac 2.
#[test]
fn an_unseated_signers_push_is_refused_and_its_peer_is_not_authenticated() {
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0)], 1).unwrap();
    let network = *genesis.id();
    let receiver = custodied(
        &lab,
        0,
        "receiver",
        log_of(std::slice::from_ref(&genesis)),
        &log_of(&[genesis]),
    );
    let paths = receiver.paths.clone().unwrap();
    let admit = |seed: [u8; 32], incarnation: [u8; 16]| {
        Draft {
            network,
            sequence: 1,
            previous: None,
            parents: vec![network],
            authored_ms: 2_000,
            body: Body::Admit {
                entrant: entrant(&lab, 1),
                evidence: aterm_secrets::membership_log::Evidence::Phrase,
            },
        }
        .sign(&seed, incarnation)
        .unwrap()
    };
    let before = client::load_records(&paths).unwrap().inventory();
    let unseated = admit([77; 32], [78; 16]);
    assert!(
        push_one(&lab, &receiver, &unseated)
            .authenticated_actor()
            .is_none()
    );
    assert_eq!(client::load_records(&paths).unwrap().inventory(), before);

    // Control: the founder's own Admit seats Mac 2.
    assert_eq!(
        push_one(&lab, &receiver, &admit([31; 32], [3; 16])).authenticated_actor(),
        Some(lab.requests[1].binding().actor())
    );
}

/// A peer this Mac refuses leaves none of its push in the Mac's records: Mac 2, a founder the
/// newest Commons epoch leaves out, pushes a record of its own, valid in itself, and is refused,
/// the push judged in a copy of the records and never written.
///
/// NEGATIVE CONTROL (in the test): the mutant that stores a push before judging it
/// ([`mutants::Mutant::StoresPushFirst`]) leaves that record in the Mac's records.
#[test]
fn a_refused_peers_push_leaves_the_receivers_records_unchanged() {
    use aterm_secrets::membership_log::EpochAudience;
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0), entrant(&lab, 1)], 1).unwrap();
    let network = *genesis.id();
    let mut held = log_of(std::slice::from_ref(&genesis));
    let actor = lab.requests[0].binding().actor();
    let (draft, key) = held
        .draft_epoch(&actor, 2, EpochAudience::Commons, 1)
        .unwrap();
    let epoch = draft.sign(&[31; 32], [3; 16]).unwrap();
    let epoch_id = *epoch.id();
    held.insert(epoch).unwrap();
    let wrap = held
        .draft_wrap(&actor, 3, epoch_id, &key, lab.identities[0].recipient())
        .unwrap()
        .sign(&[31; 32], [3; 16])
        .unwrap();
    held.insert(wrap).unwrap();
    let own = chain(network, 1, network, None, 1).remove(0);
    let receiver = custodied(&lab, 0, "receiver", log_of(&[genesis]), &held);
    let paths = receiver.paths.clone().unwrap();
    let before = client::load_records(&paths).unwrap().inventory();
    assert!(!before.contains(own.id()));

    assert!(
        push_one(&lab, &receiver, &own)
            .authenticated_actor()
            .is_none()
    );
    assert_eq!(client::load_records(&paths).unwrap().inventory(), before);

    // Control: stored before it is judged, the refused peer's record stays.
    let mutant = mutants::with(mutants::Mutant::StoresPushFirst, || {
        push_one(&lab, &receiver, &own)
    });
    assert!(mutant.authenticated_actor().is_none());
    assert!(client::load_records(&paths).unwrap().contains(own.id()));
}

/// Two Macs holding about 12,000 records each, 11,800 in common and 200 apiece the other lacks,
/// converge in one `atdb/5` session, writing beside the records they move less than an eighth of
/// one inventory of either log, where the older protocols' membership rounds sent both
/// inventories every round. Each states its frontier and ancestry checkpoints, pushes no evidence
/// (each holds the other's seat) and moves its records in one bounded batch; beside them it writes
/// its metadata and frontiers and the shared records past the last checkpoint it names that its
/// peer holds, which the checkpoints' doubling spacing lets the peer send again. The logs pass
/// 4,096 records, so the test runs under a raised held total ([`RAISED`],
/// [`RecordBudgets::within`]), as the activation raises it; their union stays within the keyed
/// budget its `Name` records count in.
///
/// NEGATIVE CONTROL (in the test): one frame naming a side's whole inventory, what an older round
/// sent from each side, breaks the bound alone.
#[test]
fn two_peers_of_twelve_thousand_records_converge_with_session_bytes_pinned() {
    const SHARED: usize = 11_800;
    const OWN: usize = 200;
    RAISED.within(|| {
        let fixture = RecordsLab::new();
        let genesis = fixture.log.genesis().clone();
        let network = *genesis.id();
        let shared = chain(network, 0, network, None, SHARED);
        let last = *shared.last().unwrap().id();
        let lefts = chain(network, 0, last, Some((last, SHARED as u64)), OWN);
        let rights = chain(network, 1, last, None, OWN);
        let mut common = vec![genesis];
        common.extend(shared);
        let side = |own: &[Record]| {
            let mut records = common.clone();
            records.extend(own.iter().cloned());
            log_of(&records)
        };
        keep(&fixture.paths[0], &side(&lefts));
        keep(&fixture.paths[1], &side(&rights));
        let moved: usize = lefts
            .iter()
            .chain(&rights)
            .map(|record| 4 + record.bytes().len())
            .sum();

        let ([(dialed, dialing), (answered, answering)], written) =
            counted_session(&fixture.endpoints[0], &fixture.endpoints[1], RAISED);
        assert!(
            dialed.is_ok() && answered.is_ok(),
            "{dialed:?} {answered:?}"
        );
        for repair in [&dialing, &answering] {
            assert!(repair.membership_error().is_none(), "{repair:?}");
            assert_eq!(repair.membership().map(|report| report.deferred), Some(0));
        }
        let union: BTreeSet<_> = common
            .iter()
            .chain(&lefts)
            .chain(&rights)
            .map(|record| *record.id())
            .collect();
        for paths in &fixture.paths {
            assert_eq!(client::load_records(paths).unwrap().inventory(), union);
        }
        let inventory = 4 + 32 * (common.len() + OWN);
        assert!(written > moved);
        let beside = written - moved;
        eprintln!(
            "session: {} records in the union, {moved} record bytes moved, {written} written, \
             {beside} beside the records, one inventory {inventory}",
            union.len()
        );
        assert!(
            beside < inventory / 8,
            "{beside} bytes beside the records; one inventory is {inventory}"
        );

        // Control: a side's whole inventory in one frame is past the bound by itself.
        let ids: Vec<Digest> = side(&lefts).records().map(|record| *record.id()).collect();
        assert!(4 + encode_frontier(RecordsBound::here(true), &ids).len() >= inventory / 8);
    });
}

/// Whether a 0.153 listener takes `frame`, the first a dialer sends after its metadata, as the
/// header of a whole witness of `records` records: it reads at most 22 bytes, and takes an `AEV4`
/// header of records, one or more.
fn released_takes(frame: &[u8], records: usize) -> bool {
    frame.len() == 22
        && &frame[..4] == b"AEV4"
        && frame[9] == 1
        && u32::from_be_bytes(frame[10..14].try_into().unwrap()) as usize == records
        && records > 0
}

/// A 0.153 Mac and this build meet on `atdb/4` frame for frame: after its metadata each sends its
/// whole seat witness in one `AEV4` evidence frame, stating nothing of what it holds, and each
/// takes the other's. A 0.153 dialer's frames (its metadata, then its whole witness) authenticate
/// it to this build's listener, which answers with its own whole witness; and the frame this
/// build's dialer writes after its metadata is one a 0.153 listener takes.
///
/// NEGATIVE CONTROL (in the test): forced onto the `atdb/4` name, the `atdb/5` wire refuses the
/// same 0.153 frames, reading their evidence header as a statement of what the peer holds, and
/// writes after its metadata a frame a 0.153 listener refuses, as it would on any release that
/// offers `atdb/4` first.
#[test]
fn a_released_atdb4_mac_and_this_build_meet_on_its_frames() {
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0), entrant(&lab, 1)], 1).unwrap();
    let witness = log_of(std::slice::from_ref(&genesis));
    let (listener, dialer) = (
        witnessed(&lab, 0, witness.clone()),
        witnessed(&lab, 1, witness.clone()),
    );
    let bound = RecordsBound::here(false);
    let metadata = |n: usize| {
        Metadata {
            request: lab.requests[n].clone(),
            bound,
            flags: 0,
        }
        .encode(lab.trust)
    };
    let mut released = Vec::new();
    send_frame(&mut released, &metadata(1)).unwrap();
    send_frame(
        &mut released,
        &evidence_header(bound, true, 1, genesis.bytes().len() as u64),
    )
    .unwrap();
    send_frame(&mut released, genesis.bytes()).unwrap();
    let answer = |wire| {
        let mut stream = Script {
            input: Cursor::new(released.clone()),
            output: Vec::new(),
        };
        let mut repair = Repair::default();
        let result = listener.connected_until(
            &mut stream,
            Some(&proof_of(&dialer)),
            Side::Responder,
            &mut repair,
            Instant::now() + EXCHANGE,
            Protocol::Frontier(wire),
        );
        (result, repair, Cursor::new(stream.output))
    };
    let (_, repair, mut answered) = answer(Wire::Whole);
    assert_eq!(
        repair.authenticated_actor(),
        Some(lab.requests[1].binding().actor())
    );
    assert_eq!(
        receive_frame(&mut answered, HELLO_FIXED + 6).unwrap(),
        metadata(0)
    );
    let header = receive_frame(&mut answered, 22).unwrap();
    assert_eq!(
        header,
        evidence_header(bound, true, 1, genesis.bytes().len() as u64)
    );
    assert_eq!(
        receive_frame(&mut answered, genesis.bytes().len()).unwrap(),
        genesis.bytes()
    );

    let opened = |wire| {
        let mut input = Vec::new();
        send_frame(&mut input, &metadata(0)).unwrap();
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let _ = dialer.connected_until(
            &mut stream,
            Some(&proof_of(&listener)),
            Side::Initiator,
            &mut Repair::default(),
            Instant::now() + EXCHANGE,
            Protocol::Frontier(wire),
        );
        let mut output = Cursor::new(stream.output);
        assert_eq!(
            receive_frame(&mut output, HELLO_FIXED + 6).unwrap(),
            metadata(1)
        );
        receive_frame(&mut output, MAX_EVIDENCE).unwrap()
    };
    assert!(released_takes(&opened(Wire::Whole), witness.held_count()));

    // Control: the `atdb/5` wire on the same name refuses 0.153's frames and sends a frame 0.153
    // refuses.
    let (result, repair, _) = answer(Wire::Delta);
    let refusal = result.unwrap_err().to_string();
    assert!(refusal.contains("invalid Commons frontier"), "{refusal}");
    assert!(repair.authenticated_actor().is_none());
    assert!(!released_takes(&opened(Wire::Delta), witness.held_count()));
}

/// One session between `fixture`'s two Macs over native TLS, Mac 0 dialing, Mac `released`
/// offering only what 0.153 offers (`atdb/4` at most) and any other Mac this build's protocols:
/// the protocol each side negotiated, what it returned and its repair, the dialer's first.
fn released_session(
    fixture: &RecordsLab,
    released: Option<usize>,
) -> [(Option<Vec<u8>>, Result<Report>, Repair); 2] {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let secret = FleetSecret::new(&[51; 32]).unwrap();
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let (tcp, _) = Connection::admit(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
                Scope::Loopback,
            )
            .unwrap()
            .into_parts();
            tcp.set_read_timeout(Some(EXCHANGE)).unwrap();
            tcp.set_write_timeout(Some(EXCHANGE)).unwrap();
            let handshake = if released == Some(1) {
                fleet::test_server_handshake_database_v4_as
            } else {
                fleet::server_handshake_database_as
            };
            let mut channel =
                handshake(tcp, &secret, Some(&fixture.endpoints[1].identity), EXCHANGE).unwrap();
            let alpn = channel
                .transport
                .stream()
                .conn
                .alpn_protocol()
                .map(<[u8]>::to_vec);
            let mut repair = Repair::default();
            let result = fixture.endpoints[1].connected_until(
                channel.transport.stream(),
                channel.peer.as_ref(),
                Side::Responder,
                &mut repair,
                Instant::now() + EXCHANGE,
                Protocol::from_alpn(alpn.as_deref()),
            );
            (alpn, result, repair)
        });
        let (tcp, _) = Connection::connect(address, Scope::Loopback, EXCHANGE)
            .unwrap()
            .into_parts();
        tcp.set_read_timeout(Some(EXCHANGE)).unwrap();
        tcp.set_write_timeout(Some(EXCHANGE)).unwrap();
        let handshake = if released == Some(0) {
            fleet::test_client_handshake_database_v4_as
        } else {
            fleet::client_handshake_database_as
        };
        let mut channel =
            handshake(tcp, &secret, Some(&fixture.endpoints[0].identity), EXCHANGE).unwrap();
        let alpn = channel
            .transport
            .stream()
            .conn
            .alpn_protocol()
            .map(<[u8]>::to_vec);
        let mut repair = Repair::default();
        let result = fixture.endpoints[0].connected_until(
            channel.transport.stream(),
            channel.peer.as_ref(),
            Side::Initiator,
            &mut repair,
            Instant::now() + EXCHANGE,
            Protocol::from_alpn(alpn.as_deref()),
        );
        [(alpn, result, repair), answered.join().unwrap()]
    })
}

/// A Mac that offers only what 0.153 offers and a Mac of this build negotiate `atdb/4` over
/// native TLS, whichever of them dials, and complete a session on it: each takes the other's
/// whole witness, and the membership rounds move both forks of the log, more records than one
/// 0.153 batch holds, until both hold the same records.
///
/// NEGATIVE CONTROL (in the test): two Macs of this build negotiate `atdb/5`, so the sessions
/// above ran the released wire, not this build's.
#[test]
fn a_released_atdb4_mac_and_this_build_complete_a_session() {
    for released in [0, 1] {
        let fixture = RecordsLab::new();
        let left = fixture.names(0, WHOLE_BATCH_RECORDS as u64 + 44);
        let right = fixture.names(1, 7);
        let expected: BTreeSet<_> = left
            .records()
            .chain(right.records())
            .map(|record| *record.id())
            .collect();
        for (alpn, result, repair) in released_session(&fixture, Some(released)) {
            assert_eq!(alpn.as_deref(), Some(fleet::ALPN_DATABASE_FRONTIER));
            assert!(result.is_ok(), "released {released}: {result:?}");
            assert!(repair.membership_error().is_none(), "{repair:?}");
            assert_eq!(repair.phase(), Phase::ControlComplete);
        }
        for paths in &fixture.paths {
            assert_eq!(client::load_records(paths).unwrap().inventory(), expected);
        }
    }

    // Control: this build's two sides meet on its own wire.
    let fixture = RecordsLab::new();
    for (alpn, result, _) in released_session(&fixture, None) {
        assert_eq!(alpn.as_deref(), Some(fleet::ALPN_DATABASE_DELTA));
        assert!(result.is_ok(), "{result:?}");
    }
}

/// A Mac at its held total still authenticates a peer its own records seat when the peer's push
/// carries records it has no room for. Mac 2's seat evidence is the whole log: the founder's
/// records, an epoch with Wraps to both Macs, Mac 2's `Admit`, and a later epoch with its Wraps.
/// The founder's Mac, full with the first five, lacks the later epoch, is pushed it and its
/// Wraps, leaves them out, and judges Mac 2 on its own records, which seat it.
///
/// NEGATIVE CONTROL (in the test): the mutant that refuses a whole push at a record its receiver
/// has no room for ([`mutants::Mutant::CapacityRefusesPush`], as the overlay did before this fix)
/// refuses Mac 2.
#[test]
fn a_full_mac_judges_a_peer_on_the_push_it_has_room_for() {
    use aterm_secrets::membership_log::EpochAudience;
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0)], 1).unwrap();
    let actor = lab.requests[0].binding().actor();
    let mut log = log_of(std::slice::from_ref(&genesis));
    let sign = |draft: Draft| draft.sign(&[31; 32], [3; 16]).unwrap();
    let epoch = |log: &mut Log, authored, generation, to: &[usize]| {
        let (draft, key) = log
            .draft_epoch(&actor, authored, EpochAudience::Everyone, generation)
            .unwrap();
        let epoch = sign(draft);
        let id = *epoch.id();
        log.insert(epoch).unwrap();
        for (n, recipient) in to.iter().enumerate() {
            let wrap = log
                .draft_wrap(
                    &actor,
                    authored + 1 + n as u64,
                    id,
                    &key,
                    lab.identities[*recipient].recipient(),
                )
                .unwrap();
            log.insert(sign(wrap)).unwrap();
        }
        (id, key)
    };
    let (first, first_key) = epoch(&mut log, 2, 1, &[0]);
    log.insert(sign(log.draft(
        &actor,
        4,
        Body::Admit {
            entrant: entrant(&lab, 1),
            evidence: aterm_secrets::membership_log::Evidence::Phrase,
        },
    )))
    .unwrap();
    let wrap = log
        .draft_wrap(&actor, 5, first, &first_key, lab.identities[1].recipient())
        .unwrap();
    log.insert(sign(wrap)).unwrap();
    let held: Vec<Record> = log.records().cloned().collect();
    epoch(&mut log, 6, 2, &[0, 1]);
    let pushed: Vec<Record> = log.records().skip(held.len()).cloned().collect();
    assert_eq!((held.len(), pushed.len()), (5, 3));

    let full = RecordBudgets {
        held: held.len(),
        ..RecordBudgets::PRODUCTION
    };
    full.within(|| {
        let receiver = custodied(&lab, 0, "full", log_of(&held[..1]), &log_of(&held));
        let paths = receiver.paths.clone().unwrap();
        let before = client::load_records(&paths).unwrap().inventory();
        let bound = RecordsBound::here(false);
        let mut input = opening(lab.trust, &lab.requests[1], bound, Wire::Delta);
        let bytes: usize = pushed.iter().map(|record| record.bytes().len()).sum();
        send_frame(
            &mut input,
            &evidence_header(bound, true, pushed.len() as u32, bytes as u64),
        )
        .unwrap();
        for record in &pushed {
            send_frame(&mut input, record.bytes()).unwrap();
        }
        let judge = || {
            let mut stream = Script {
                input: Cursor::new(input.clone()),
                output: Vec::new(),
            };
            let mut repair = Repair::default();
            let _ = receiver.connected_until(
                &mut stream,
                Some(&proof_of(&lab.endpoint(1))),
                Side::Responder,
                &mut repair,
                Instant::now() + EXCHANGE,
                Protocol::Frontier(Wire::Delta),
            );
            repair.authenticated_actor()
        };
        assert_eq!(judge(), Some(lab.requests[1].binding().actor()));
        assert_eq!(client::load_records(&paths).unwrap().inventory(), before);

        // Control: refused whole at the first record it has no room for, the push refuses Mac 2.
        assert_eq!(
            mutants::with(mutants::Mutant::CapacityRefusesPush, judge),
            None
        );
    });
}

/// One session from `initiator` to `responder` under `wire` over loopback, with `mutant` on in
/// the responder's thread: what the responder returned, and its repair.
fn answered(
    initiator: &Endpoint,
    responder: &Endpoint,
    wire: Wire,
    unread: bool,
    mutant: Option<mutants::Mutant>,
) -> (Result<Report>, Repair) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let run = |endpoint: &Endpoint, peer: &Endpoint, mut stream: TcpStream, side: Side| {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let mut repair = Repair::default();
        let result = endpoint.connected_until(
            &mut stream,
            Some(&proof_of(peer)),
            side,
            &mut repair,
            Instant::now() + EXCHANGE,
            Protocol::Frontier(wire),
        );
        (result, repair)
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0;
            let answer = || run(responder, initiator, stream, Side::Responder);
            let checked = || match mutant {
                Some(mutant) => mutants::with(mutant, answer),
                None => answer(),
            };
            if unread {
                crate::control::faults::records_unread(checked)
            } else {
                checked()
            }
        });
        let _ = run(
            initiator,
            responder,
            TcpStream::connect(address).unwrap(),
            Side::Initiator,
        );
        answered.join().unwrap()
    })
}

/// Lab Macs 1 and 2 founded together, and Mac 1's `Remove` of Mac 2 after the genesis: the
/// genesis log, which seats Mac 2 and is all its own evidence, and the log holding the `Remove`.
fn founded_then_removed(lab: &Lab) -> (Log, Log) {
    use aterm_secrets::membership_log::EpochAudience;
    let genesis = Record::found(&ROOT, vec![entrant(lab, 0), entrant(lab, 1)], 1).unwrap();
    let mut founded = log_of(std::slice::from_ref(&genesis));
    let actor = lab.requests[0].binding().actor();
    let (draft, key) = founded
        .draft_epoch(&actor, 2, EpochAudience::Commons, 1)
        .unwrap();
    let epoch = draft.sign(&[31; 32], [3; 16]).unwrap();
    let epoch_id = *epoch.id();
    founded.insert(epoch).unwrap();
    for identity in &lab.identities {
        let wrap = founded
            .draft_wrap(&actor, 3, epoch_id, &key, identity.recipient())
            .unwrap()
            .sign(&[31; 32], [3; 16])
            .unwrap();
        founded.insert(wrap).unwrap();
    }
    let mut removed = founded.clone();
    let remove = removed
        .draft(
            &lab.requests[0].binding().actor(),
            2,
            Body::Remove {
                member: lab.requests[1].binding().actor(),
                cutoff: *genesis.id(),
            },
        )
        .sign(&[31; 32], [3; 16])
        .unwrap();
    removed.insert(remove).unwrap();
    (founded, removed)
}

/// X1: a Mac this Mac removed is refused at the handshake while this Mac's membership records do
/// not read, under either wire. Its own evidence (the genesis) seats it; only this Mac's records
/// say it is out, so a check that cannot read them refuses rather than passing it. Every read of
/// the responder's records by a peer check fails for the whole session
/// ([`crate::control::faults::records_unread`], as a read fails while a writer replaces the file
/// under it), while its roster and deny list read as they do: the bad interleaving, held for every
/// check of the session. With the records read it is refused too, and a Mac they do not remove is
/// let in.
///
/// Measured before the fix: `two_islands_admit_and_remove_then_tables_continue_after_the_heal`
/// failed 1 run in 3 on m20, its responder completing a session with the Mac it had just revoked.
/// The session's view read the records with `load_records(..).ok()`, judging a push from its own
/// genesis when they did not read, and the local log check answered `Ok(())` when
/// `trusted_commons_log` failed, so one moment the records did not read passed every check.
///
/// NEGATIVE CONTROL (in the test): with a failed read passing the peer
/// ([`mutants::Mutant::UnreadCustodyAdmits`], as both checks did before X1), the removed Mac is
/// authenticated and the responder's session completes, under both wires.
#[test]
fn a_removed_mac_is_refused_while_its_responders_records_do_not_read() {
    let lab = Lab::new();
    drop(lab.journal(1));
    let (founded, removed) = founded_then_removed(&lab);
    let mac2 = witnessed(&lab, 1, founded.clone());
    let actor = lab.requests[1].binding().actor();
    let kept = custodied(&lab, 0, "kept", founded.clone(), &founded);
    let responder = custodied(&lab, 0, "responder", founded, &removed);
    for wire in [Wire::Delta, Wire::Whole] {
        let (result, repair) = answered(&mac2, &kept, wire, false, None);
        assert!(
            result.is_ok(),
            "{wire:?}: a Mac no record removes: {result:?}"
        );
        assert_eq!(repair.authenticated_actor(), Some(actor), "{wire:?}");
        let (result, repair) = answered(&mac2, &responder, wire, false, None);
        assert!(result.is_err(), "{wire:?}: the records it reads refuse it");
        assert_eq!(repair.authenticated_actor(), None, "{wire:?}");

        let (result, repair) = answered(&mac2, &responder, wire, true, None);
        assert!(
            result.is_err(),
            "{wire:?}: a responder whose records do not read refuses the Mac they remove"
        );
        assert_eq!(repair.authenticated_actor(), None, "{wire:?}");

        let (result, repair) = answered(
            &mac2,
            &responder,
            wire,
            true,
            Some(mutants::Mutant::UnreadCustodyAdmits),
        );
        assert!(
            result.is_ok(),
            "CONTROL {wire:?}: a failed read passed the removed Mac: {result:?}"
        );
        assert_eq!(
            repair.authenticated_actor(),
            Some(actor),
            "CONTROL {wire:?}"
        );
    }
}

/// X1: a `Remove` this Mac made durable and has not acknowledged yet refuses its Mac at the
/// handshake, under either wire: a writer cut short between writing the records file and
/// acknowledging it (or whose acknowledgement failed) leaves the acknowledged history without it,
/// and the local log check reads every record held, as admission holds out what records ahead
/// of their acknowledgement hold out. The records file is written here as that writer leaves it,
/// so the `Remove` is ahead of the acknowledged history at every check.
///
/// NEGATIVE CONTROL (in the test): judging by the acknowledged records alone
/// ([`mutants::Mutant::AcknowledgedOnly`], as the check did before X1) authenticates the removed
/// Mac under `atdb/4`, where no view of the records judges its push.
#[test]
fn a_remove_not_yet_acknowledged_refuses_its_mac_at_the_handshake() {
    let lab = Lab::new();
    drop(lab.journal(1));
    let (founded, removed) = founded_then_removed(&lab);
    let mac2 = witnessed(&lab, 1, founded.clone());
    let actor = lab.requests[1].binding().actor();
    let responder = custodied(&lab, 0, "responder", founded.clone(), &founded);
    let paths = responder.paths.clone().unwrap();
    aterm_secrets::fsx::write_atomic(
        &paths.records_file(),
        &encode_records(removed.records()),
        0o600,
    )
    .unwrap();
    let read = client::load_acknowledged_and_ahead(&paths).unwrap();
    let heads = |store: &aterm_secrets::membership_log::Store| store.log().map(Log::heads);
    assert_eq!(
        heads(&read.acknowledged),
        Some(founded.heads()),
        "the acknowledged history holds no Remove"
    );
    assert_eq!(
        read.ahead.as_ref().and_then(heads),
        Some(removed.heads()),
        "the Remove is durable, ahead of it"
    );
    assert_eq!(
        client::trusted_commons_log(&paths, lab.trust)
            .unwrap()
            .map(|(log, _)| log.heads()),
        Some(founded.heads()),
        "the log the check judges by is the acknowledged one"
    );
    for wire in [Wire::Delta, Wire::Whole] {
        let (result, repair) = answered(&mac2, &responder, wire, false, None);
        assert!(result.is_err(), "{wire:?}: the durable Remove refuses it");
        assert_eq!(repair.authenticated_actor(), None, "{wire:?}");
    }
    let (result, repair) = answered(
        &mac2,
        &responder,
        Wire::Whole,
        false,
        Some(mutants::Mutant::AcknowledgedOnly),
    );
    assert!(
        result.is_ok(),
        "CONTROL: the acknowledged records alone passed the removed Mac: {result:?}"
    );
    assert_eq!(repair.authenticated_actor(), Some(actor), "CONTROL");
}

/// One session from `initiator` to `responder` under `wire` over loopback, with the responder's
/// checks and grants taped ([`crate::control::peer_check::tape`]): what it returned, its repair,
/// and its tape.
fn taped(
    initiator: &Endpoint,
    responder: &Endpoint,
    wire: Wire,
) -> (
    Result<Report>,
    Repair,
    Vec<crate::control::peer_check::tape::Step>,
) {
    responded(initiator, responder, wire, |answer| {
        let ((result, repair), tape) = crate::control::peer_check::tape::recorded(answer);
        (result, repair, tape)
    })
}

/// The responder's side of a session: what it returned, and its repair.
type Answer<'a> = Box<dyn FnOnce() -> (Result<Report>, Repair) + 'a>;

/// One session from `initiator` to `responder` under `wire` over loopback, the responder's side
/// answered inside `within`, on the responder's own thread, which is handed it to call: what
/// `within` gave.
fn responded<W: Send>(
    initiator: &Endpoint,
    responder: &Endpoint,
    wire: Wire,
    within: impl FnOnce(Answer<'_>) -> W + Send,
) -> W {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let run = |endpoint: &Endpoint, peer: &Endpoint, mut stream: TcpStream, side: Side| {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let mut repair = Repair::default();
        let result = endpoint.connected_until(
            &mut stream,
            Some(&proof_of(peer)),
            side,
            &mut repair,
            Instant::now() + EXCHANGE,
            Protocol::Frontier(wire),
        );
        (result, repair)
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0;
            within(Box::new(|| {
                run(responder, initiator, stream, Side::Responder)
            }))
        });
        let _ = run(
            initiator,
            responder,
            TcpStream::connect(address).unwrap(),
            Side::Initiator,
        );
        answered.join().unwrap()
    })
}

/// Replay `tape` through the handshake kernel, as the island's session model steps
/// (`crates/aterm-db-core/src/peer_check_island.rs`): each read is of the stage the kernel named,
/// a check passes or refuses where the kernel says, at a log endpoint a passing check read every
/// record held and found them clear, and each grant spends the one pass the check before it
/// minted. The grants the tape holds.
fn replay(tape: &[crate::control::peer_check::tape::Step]) -> usize {
    use crate::control::peer_check::tape::Step;
    use aterm_db_core::peer_check::{Custody, Read, Stage};
    use aterm_db_core::{peer_check_next as next, peer_check_start as start};
    let (mut check, mut unspent, mut grants, mut refused) = (None, 0, 0, false);
    for step in tape {
        match *step {
            Step::Begin(custody, witness) => {
                assert!(check.is_none(), "a check began inside another: {tape:?}");
                check = Some((custody, witness, start(custody), false));
            }
            Step::Read(stage, outcome) => {
                let (custody, witness, at, held) = check.expect("a read outside a check");
                assert_eq!(stage, at, "the driver read a stage the kernel did not name");
                let held = held || (stage == Stage::Held as u8 && outcome == Read::Clear as u8);
                check = Some((
                    custody,
                    witness,
                    next(custody, stage, outcome, witness),
                    held,
                ));
            }
            Step::Pass => {
                let (custody, _, at, held) = check.take().expect("a pass outside a check");
                assert_eq!(at, Stage::Pass as u8, "a pass the kernel did not reach");
                assert!(
                    custody != Custody::Log as u8 || held,
                    "a log endpoint passed without its held records"
                );
                unspent += 1;
                assert_eq!(unspent, 1, "two passes in hand: {tape:?}");
            }
            Step::Refuse => {
                let (_, _, at, _) = check.take().expect("a refusal outside a check");
                assert_eq!(
                    at,
                    Stage::Refuse as u8,
                    "a refusal the kernel did not reach"
                );
                refused = true;
            }
            Step::Grant => {
                assert!(!refused, "a grant after a refusal: {tape:?}");
                assert_eq!(unspent, 1, "a grant with no fresh pass: {tape:?}");
                unspent -= 1;
                grants += 1;
            }
        }
    }
    assert!(check.is_none(), "a check left unfinished: {tape:?}");
    grants
}

/// PF4: real sessions follow the island's event order. A responder's every check walks the
/// stages the kernel names, a log endpoint passes a peer only on its held records read clear, and
/// every grant (the lease, its own evidence, control, membership records, the application) spends
/// the one pass a fresh check minted, under either wire; a Mac its records remove is refused at
/// its first check, with no grant at all.
#[test]
fn every_grant_of_a_real_session_spends_one_fresh_pass() {
    let lab = Lab::new();
    drop(lab.journal(1));
    let (founded, removed) = founded_then_removed(&lab);
    let mac2 = witnessed(&lab, 1, founded.clone());
    let kept = custodied(&lab, 0, "kept", founded.clone(), &founded);
    let responder = custodied(&lab, 0, "responder", founded, &removed);
    for wire in [Wire::Delta, Wire::Whole] {
        let (result, _, tape) = taped(&mac2, &kept, wire);
        assert!(result.is_ok(), "{wire:?}: {result:?}");
        let grants = replay(&tape);
        assert!(
            grants >= 3,
            "{wire:?}: the lease, the evidence and control: {tape:?}"
        );

        let (result, repair, tape) = taped(&mac2, &responder, wire);
        assert!(result.is_err(), "{wire:?}: the records it reads refuse it");
        assert_eq!(repair.authenticated_actor(), None, "{wire:?}");
        assert_eq!(
            replay(&tape),
            0,
            "{wire:?}: a refused Mac is granted nothing"
        );
    }
}

/// Lab Macs 1 and 2 founded together ([`founded_then_removed`]'s genesis log): Mac 2 with that log
/// as its evidence and no records custody, and Mac 1 holding it in records custody of its own, so
/// each of Mac 1's checks of Mac 2 reads every stage of a log endpoint.
fn founded_pair(lab: &Lab) -> (Endpoint, Endpoint, Log) {
    drop(lab.journal(1));
    let (founded, _) = founded_then_removed(lab);
    let witnessed = witnessed(lab, 1, founded.clone());
    let custodied = custodied(lab, 0, "kept", founded.clone(), &founded);
    (witnessed, custodied, founded)
}

/// Whether a writer's exclusive take of the state lock of the vault at `paths`, tried once on a
/// descriptor of its own, takes it; one that does lets it go at once.
fn writer_takes(paths: &client::Paths) -> bool {
    let file = aterm_uds::private_file::open_private(&paths.dir, ".lock")
        .unwrap()
        .0;
    #[expect(
        clippy::disallowed_methods,
        reason = "a writer's one try at the vault's state lock, let go at once by File::unlock"
    )]
    let attempt = file.try_lock();
    match attempt {
        Ok(()) => {
            file.unlock().unwrap();
            true
        }
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(error) => panic!("the writer's take of the state lock failed: {error}"),
    }
}

/// P-L1: a peer check reads every stage through one look at the vault (`peer_check::check`'s
/// kept look). Over a quiet `atdb/5` session from a Mac whose evidence is its log to a Mac that
/// holds that log in records custody ([`founded_pair`]), after a session that warmed the memos,
/// each of the responder's checks opens one look on an observation of its own
/// (`client::test_looks_observed`), its files judged settled (`client::test_as_if_later`,
/// installed in the responder's thread, as it and these counts are each thread's own). The files
/// the vault's memos read are stamped through that look (`client::Derivations::vault_stamps`),
/// held to at least once and not to exactly once: in this crate's test process aterm-secrets'
/// cache of what `state.toml` showed (`STATE_LOOKS`) keeps at most sixteen folders, the oldest let
/// go first, which every test running beside this one fills, and an entry let go before a check's
/// first stamping makes that stamping read the file afresh, which the look then does not keep, so
/// its next ask stamps again. The exact count is aterm-secrets' own look laws', whose test process
/// keeps every live folder. CONTROL: each check reads four stages (the roster, every record held,
/// the acknowledged log, and that log with the peer's evidence).
///
/// NEGATIVE CONTROL (2026-11-11): with the Held stage on a look of its own again
/// (`held(&VaultLook::new(paths), ..)`), each check opens two looks.
#[test]
fn a_peer_check_reads_every_stage_through_one_look() {
    use crate::control::peer_check::tape::{self, Step};
    let lab = Lab::new();
    let (mac2, responder, _) = founded_pair(&lab);
    let session = || {
        responded(&mac2, &responder, Wire::Delta, |answer| {
            client::test_as_if_later(std::time::Duration::from_secs(3600), || {
                let (((result, _), checks), steps) =
                    tape::recorded(|| tape::probed(|_| {}, |_| {}, answer));
                (result, checks, steps)
            })
        })
    };
    let (warmed, _, _) = session();
    assert!(warmed.is_ok(), "the warming session: {warmed:?}");
    let (result, checks, steps) = session();
    assert!(result.is_ok(), "{result:?}");
    let mut reads = Vec::new();
    for step in &steps {
        match step {
            Step::Begin(..) => reads.push(0),
            Step::Read(..) => *reads.last_mut().expect("a read inside a check") += 1,
            Step::Pass | Step::Refuse | Step::Grant => {}
        }
    }
    assert!(
        !reads.is_empty() && reads.iter().all(|reads| *reads == 4),
        "CONTROL: each check reads its four stages: {steps:?}"
    );
    assert_eq!(
        checks.len(),
        reads.len(),
        "each check is counted: {checks:?}"
    );
    assert!(
        checks
            .iter()
            .all(|checked| checked.looks == 1 && checked.vault_stamps >= 1),
        "each check reads every stage through one look: {checks:?}"
    );
}

/// P-L2: no writer of the state lock enters between a peer check's stages. Wherever one of the
/// session's responder's checks ended a stage it read and has not asked for the next
/// (`peer_check::tape::between`), a writer's exclusive take of the vault's state lock, tried once
/// on a descriptor of its own ([`writer_takes`]), finds it held: the check's look holds it shared
/// from its first stage to its last. It judges only whether the vault's state lock is held at each
/// boundary, which the check's kept look decides whatever its memos keep, so it needs no settled
/// files (`client::test_as_if_later`) and no warming session. CONTROL: the same take after the
/// session takes the lock.
///
/// NEGATIVE CONTROL (2026-11-11): with each stage on a look of its own again, let go at its arm's
/// end (`&VaultLook::new(paths)` in each arm), the take takes the lock at every boundary.
#[test]
fn no_state_lock_writer_enters_between_a_peer_checks_stages() {
    use crate::control::peer_check::tape;
    let lab = Lab::new();
    let (mac2, responder, _) = founded_pair(&lab);
    let (result, boundaries) = responded(&mac2, &responder, Wire::Delta, |answer| {
        let taken = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let between = {
            let taken = taken.clone();
            move |paths: &client::Paths| taken.borrow_mut().push(writer_takes(paths))
        };
        let ((result, _), _) = tape::probed(|_| {}, between, answer);
        (result, taken.take())
    });
    assert!(result.is_ok(), "{result:?}");
    assert!(
        !boundaries.is_empty(),
        "CONTROL: the responder's checks read stages"
    );
    assert!(
        boundaries.iter().all(|taken| !taken),
        "a writer took the vault's state lock between a check's stages: {boundaries:?}"
    );
    assert!(
        writer_takes(responder.paths.as_ref().unwrap()),
        "CONTROL: the same take after the session takes the lock"
    );
}

/// P-L3: a stage after a look that holds nothing takes a look of its own. A writer holds the
/// vault's state lock exclusively as a check begins, so the look its Roster stage takes observes
/// nothing (the vault is busy), and lets go once that look is taken
/// (`peer_check::tape::look_taken`): the check passes, through two looks, the Held stage's its own
/// (`client::test_looks_observed`). CONTROL: with no writer the same check passes through one.
///
/// NEGATIVE CONTROL (2026-11-11): with `peer_check::through` keeping any look it holds, whether or
/// not it holds the vault (its `holds_vault` test left out), the contended check passes through
/// one look.
#[test]
fn a_stage_after_a_look_that_holds_nothing_takes_a_look_of_its_own() {
    use crate::control::peer_check::{self, Judged, tape};
    let lab = Lab::new();
    let (_, responder, founded) = founded_pair(&lab);
    let judged = Judged::Peer {
        request: &lab.requests[1],
        log: Some(&founded),
    };
    let checked = |writer: Option<std::fs::File>| {
        let mut writer = writer;
        let before = client::test_looks_observed();
        let (passed, checks) = tape::probed(
            move |_| {
                if let Some(writer) = writer.take() {
                    writer.unlock().unwrap();
                }
            },
            |_| {},
            || peer_check::check(responder.custody(), responder.trust, &judged),
        );
        (
            passed.map(peer_check::Pass::waive),
            client::test_looks_observed() - before,
            checks,
        )
    };
    let (passed, looks, checks) = checked(None);
    assert!(passed.is_ok(), "CONTROL: {passed:?}");
    assert_eq!(looks, 1, "CONTROL: one look with no writer: {checks:?}");
    let writer =
        aterm_uds::private_file::open_private(&responder.paths.as_ref().unwrap().dir, ".lock")
            .unwrap()
            .0;
    #[expect(
        clippy::disallowed_methods,
        reason = "a writer holds the vault's state lock as the check begins; its probe lets go"
    )]
    writer.try_lock().unwrap();
    let (passed, looks, checks) = checked(Some(writer));
    assert!(passed.is_ok(), "{passed:?}");
    assert_eq!(
        looks, 2,
        "a stage after a look that holds nothing takes its own: {checks:?}"
    );
}

/// What a test names `protocol` by: its ALPN, or none.
const fn alpn(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Legacy => "no ALPN",
        Protocol::Repair => "atdb/2",
        Protocol::Credentials => "atdb/3",
        Protocol::Frontier(Wire::Whole) => "atdb/4",
        Protocol::Frontier(Wire::Delta) => "atdb/5",
    }
}

/// One session from `initiator` to `responder` over loopback under `protocol`, with `mutant` on
/// and `hook` observing the peer cache in the responder's thread: what each side returned and its
/// repair, the responder's first.
fn session(
    initiator: &Endpoint,
    responder: &Endpoint,
    protocol: Protocol,
    mutant: Option<mutants::Mutant>,
    hook: impl FnMut(PeerCacheStage) + Send + 'static,
) -> [(Result<Report>, Repair); 2] {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let run = |endpoint: &Endpoint, peer: &Endpoint, mut stream: TcpStream, side: Side| {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let mut repair = Repair::default();
        let result = endpoint.connected_until(
            &mut stream,
            Some(&proof_of(peer)),
            side,
            &mut repair,
            Instant::now() + EXCHANGE,
            protocol,
        );
        (result, repair)
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0;
            let answer = || run(responder, initiator, stream, Side::Responder);
            observe_peer_cache(hook, || match mutant {
                Some(mutant) => mutants::with(mutant, answer),
                None => answer(),
            })
        });
        let dialed = run(
            initiator,
            responder,
            TcpStream::connect(address).unwrap(),
            Side::Initiator,
        );
        [answered.join().unwrap(), dialed]
    })
}

/// PF4: a member serves a pending device's enrollment only while every membership record it
/// holds lets that device in. Mac 2 enrolls, pending, from Mac 1, whose membership genesis founded
/// its log; Mac 1 then makes a `Remove` of Mac 2 durable without acknowledging it (as a writer cut
/// short between writing the records file and acknowledging it leaves the file, the records
/// written here as X1's test writes them), and the same pending device dials it again under
/// `atdb/5`, `atdb/4` and the legacy `ACE1` exchange (the one an `atdb/2` or `atdb/3` channel and
/// a channel with no ALPN carry for a pending device). Under each, Mac 1 refuses it: its session
/// fails, it never authenticates Mac 2, and it authors no record.
///
/// The defect it pins: a pending device skips the peer check. `frontier::connected` routes a
/// `PENDING` flag to `enroll` before any `verify_evidence`, and `connected_until` an `ACE1` hello
/// to `enrollment::serve`; the serving side checks only `check_log_commons_enrollment`, which
/// judges the acknowledged log ([`client::trusted_commons_log`]), so a `Remove` ahead of its
/// acknowledgement, which X1 made stand at the handshake
/// (`a_remove_not_yet_acknowledged_refuses_its_mac_at_the_handshake`), takes no one out there.
///
/// CONTROLS (in the test): the same pending device enrolls from Mac 1 under each protocol while its
/// records hold no `Remove` of it (the first enrollment authoring its seat), and once the `Remove`
/// is acknowledged it is refused under each: the guard the acknowledged log already holds, so the
/// refusal this test asks for differs only in reading every record held.
#[test]
fn a_pending_enrollment_is_judged_on_every_record_held() {
    use crate::control::enrollment::tests::Lab as Vault;
    const ENROLLING: [Protocol; 3] = [
        Protocol::Frontier(Wire::Delta),
        Protocol::Frontier(Wire::Whole),
        Protocol::Legacy,
    ];
    let vault = Vault::new(true);
    vault.found_log();
    let pending = vault.pending();
    let owner = Endpoint::from_bootstrap(&vault.bootstrap(0))
        .unwrap()
        .unwrap();
    assert!(matches!(owner.custody(), Custody::Log(_)));
    let paths = vault.paths[0].clone();
    let actor = pending.actor;
    let founded = client::load_records(&paths).unwrap().inventory();
    for protocol in ENROLLING {
        let [(served, repair), (enrolled, _)] = session(&pending, &owner, protocol, None, |_| {});
        assert!(
            served.is_ok() && enrolled.is_ok(),
            "CONTROL {}: a Mac no record removes enrolls: {served:?} {enrolled:?}",
            alpn(protocol)
        );
        assert_eq!(
            repair.authenticated_actor(),
            Some(actor),
            "CONTROL {}",
            alpn(protocol)
        );
    }
    let seated = client::load_records(&paths).unwrap();
    assert_ne!(
        seated.inventory(),
        founded,
        "CONTROL: the first enrollment authored Mac 2's seat"
    );
    let seated = seated.log().unwrap().clone();
    let entry = seated.membership().members()[&actor].entry;
    let own = |record: &&Record| {
        *record.actor() == actor && !matches!(record.body(), Body::Genesis { .. })
    };
    let cutoff = seated
        .records()
        .filter(own)
        .max_by_key(|record| record.sequence())
        .map_or(entry, |record| *record.id());
    let mut removed = seated.clone();
    let bootstrap = vault.bootstrap(0);
    let request = bootstrap.request().unwrap();
    let authored_ms = seated.records().map(Record::authored_ms).max().unwrap() + 1;
    let remove = bootstrap
        .device()
        .with_seed(|seed| {
            seated
                .draft(
                    &request.binding().actor(),
                    authored_ms,
                    Body::Remove {
                        member: actor,
                        cutoff,
                    },
                )
                .sign(seed, *request.binding().incarnation())
        })
        .unwrap();
    removed.insert(remove).unwrap();
    assert!(removed.membership().removal(&actor).is_some());
    aterm_secrets::fsx::write_atomic(
        &paths.records_file(),
        &encode_records(removed.records()),
        0o600,
    )
    .unwrap();
    let read = client::load_acknowledged_and_ahead(&paths).unwrap();
    let heads = |store: &aterm_secrets::membership_log::Store| store.log().map(Log::heads);
    assert_eq!(
        heads(&read.acknowledged),
        Some(seated.heads()),
        "the acknowledged history holds no Remove"
    );
    assert_eq!(
        read.ahead.as_ref().and_then(heads),
        Some(removed.heads()),
        "the Remove is durable, ahead of it"
    );
    let held = client::load_records(&paths).unwrap().inventory();
    let served: Vec<_> = ENROLLING
        .into_iter()
        .filter_map(|protocol| {
            let [(served, repair), (enrolled, _)] =
                session(&pending, &owner, protocol, None, |_| {});
            let authored = client::load_records(&paths).unwrap().inventory() != held;
            (served.is_ok() || repair.authenticated_actor().is_some() || authored).then(|| {
                format!(
                    "{}: served {:?}, authenticated {}, authored {authored}, device {:?}",
                    alpn(protocol),
                    served.map(|report| report.sent),
                    repair.authenticated_actor().is_some(),
                    enrolled.map(|report| report.received),
                )
            })
        })
        .collect();
    assert!(
        served.is_empty(),
        "a Remove ahead of its acknowledgement left Mac 2 an enrollment: {served:?}"
    );
    // NEGATIVE CONTROL: judged by the acknowledged log alone, as before PF4, the same pending
    // device enrolls under each protocol.
    for protocol in ENROLLING {
        let [(served, repair), _] = session(
            &pending,
            &owner,
            protocol,
            Some(mutants::Mutant::AcknowledgedOnly),
            |_| {},
        );
        assert!(
            served.is_ok() && repair.authenticated_actor() == Some(actor),
            "NEGATIVE CONTROL {}: the acknowledged log alone enrolls Mac 2: {served:?}",
            alpn(protocol)
        );
    }

    // CONTROL: the same records acknowledged, as a writer that finishes leaves them.
    aterm_secrets::fsx::write_atomic(
        &paths.records_file(),
        &encode_records(seated.records()),
        0o600,
    )
    .unwrap();
    assert_eq!(
        client::save_records(&paths, &encode_records(removed.records())).unwrap(),
        1
    );
    assert_eq!(
        heads(
            &client::load_acknowledged_and_ahead(&paths)
                .unwrap()
                .acknowledged
        ),
        Some(removed.heads()),
        "CONTROL: the Remove is acknowledged"
    );
    for protocol in ENROLLING {
        let [(served, repair), _] = session(&pending, &owner, protocol, None, |_| {});
        assert!(
            served.is_err(),
            "CONTROL {}: an acknowledged Remove refuses Mac 2: {served:?}",
            alpn(protocol)
        );
        assert_eq!(
            repair.authenticated_actor(),
            None,
            "CONTROL {}",
            alpn(protocol)
        );
    }
}

/// How a test makes a `Remove` durable in a Mac's records: as a writer cut short leaves the
/// records file, ahead of its acknowledgement, or as one that finishes leaves it.
#[derive(Clone, Copy, Debug)]
enum Durable {
    Ahead,
    Acknowledged,
}

/// What a responder granted Mac 2 in one session after its `Remove` of Mac 2 became durable.
#[derive(Debug)]
struct Later {
    /// The control nodes the responder's session reported sending, had it completed.
    answered: Option<usize>,
    /// The membership records it reported sending, had it traded any.
    membership: Option<usize>,
    /// The control nodes Mac 2's journal took.
    control: usize,
    /// Whether Mac 2's records took any.
    records: bool,
}

impl Later {
    /// Whether any grant ran.
    const fn any(&self) -> bool {
        self.answered.is_some() || self.membership.is_some() || self.control != 0 || self.records
    }
}

/// PF4: once a peer passes its check, every later grant of a session stands only while this Mac's
/// records still let it in. A responder speaking the legacy protocol (no ALPN, `atdb/2` or
/// `atdb/3`) checks Mac 2 once and passes it; then, at its first grant (the lease, where
/// [`PeerCacheStage::Acquired`] fires in its thread), its own `Remove` of Mac 2 becomes durable,
/// ahead of its acknowledgement or acknowledged. Its session must fail with no grant after: it
/// trades Mac 2 no membership records and sends it none of its control history (one node its
/// journal holds that Mac 2's lacks). `atdb/5` and `atdb/4`, which re-check before each grant,
/// stop at the same moment.
///
/// The defect it pins: `connected_until`'s legacy branch calls `verify_peer_against_local_log`
/// once, before the lease, and then syncs `ACX2` membership, repairs control, trades recovery and
/// membership records, repairs the application and hands off credentials, none re-checked: a Mac
/// removed while a legacy session runs is granted the rest of that session.
///
/// Each session asserts its hook fired, so one refused before its lease (the `Remove` not yet
/// written) cannot pass the test.
///
/// NEGATIVE CONTROLS (in the test): under `atdb/4`, with the check judging the acknowledged
/// records alone ([`mutants::Mutant::AcknowledgedOnly`]), the `Remove` written ahead of its
/// acknowledgement stops nothing, and the facts this test reads see Mac 2 granted its records and
/// the responder's control history; and under each legacy protocol, with one check passing every
/// later one ([`mutants::Mutant::CheckedOnce`], the session before PF4), so does the `Remove`
/// written either way.
#[test]
fn a_remove_after_the_check_stops_every_later_grant_on_atdb_2_3() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let lab = Lab::new();
    let (founded, removed) = founded_then_removed(&lab);
    let wire = encode_records(removed.records());
    let mut n = 0;
    // What the responder granted Mac 2 after its `Remove` became durable, `durable`, at its lease
    // under `protocol` with `mutant` on.
    let mut granted = |protocol: Protocol, durable: Durable, mutant: Option<mutants::Mutant>| {
        n += 1;
        let responder = custodied(
            &lab,
            0,
            &format!("responder-{n}"),
            founded.clone(),
            &founded,
        );
        let mac2 = custodied(&lab, 1, &format!("mac2-{n}"), founded.clone(), &founded);
        Journal::open(&responder.dir, lab.trust)
            .unwrap()
            .ingest(lab.genesis.bytes())
            .unwrap();
        let paths = responder.paths.clone().unwrap();
        let written = Arc::new(AtomicBool::new(false));
        let hook = {
            let (paths, wire, written) = (paths.clone(), wire.clone(), written.clone());
            move |stage: PeerCacheStage| {
                if matches!(stage, PeerCacheStage::Acquired(Side::Responder))
                    && !written.swap(true, Ordering::SeqCst)
                {
                    match durable {
                        Durable::Ahead => {
                            aterm_secrets::fsx::write_atomic(&paths.records_file(), &wire, 0o600)
                                .unwrap();
                        }
                        Durable::Acknowledged => {
                            assert_eq!(client::save_records(&paths, &wire).unwrap(), 1);
                        }
                    }
                }
            }
        };
        let [(answered, repair), (dialed, _)] = session(&mac2, &responder, protocol, mutant, hook);
        assert!(
            written.load(Ordering::SeqCst),
            "{} {durable:?}: the responder passed Mac 2's check and took its lease: \
             {answered:?} {dialed:?}",
            alpn(protocol)
        );
        assert_eq!(
            client::load_records(&paths).unwrap().log().map(Log::heads),
            Some(removed.heads()),
            "{} {durable:?}: the Remove is durable",
            alpn(protocol)
        );
        Later {
            answered: answered.ok().map(|report| report.sent),
            membership: repair.membership().map(|report| report.sent),
            control: Journal::open(&mac2.dir, lab.trust)
                .unwrap()
                .inventory()
                .count(),
            records: client::load_records(mac2.paths.as_ref().unwrap())
                .unwrap()
                .log()
                .is_some_and(|log| log.heads() != founded.heads()),
        }
    };
    let mut later = Vec::new();
    for protocol in [
        Protocol::Legacy,
        Protocol::Repair,
        Protocol::Credentials,
        Protocol::Frontier(Wire::Delta),
        Protocol::Frontier(Wire::Whole),
    ] {
        for durable in [Durable::Ahead, Durable::Acknowledged] {
            let seen = granted(protocol, durable, None);
            if seen.any() {
                later.push((alpn(protocol), durable, seen));
            }
        }
    }
    let control = granted(
        Protocol::Frontier(Wire::Whole),
        Durable::Ahead,
        Some(mutants::Mutant::AcknowledgedOnly),
    );
    assert!(
        control.answered.is_some() && control.control != 0 && control.records,
        "CONTROL: judged by the acknowledged records alone, the Remove ahead stops nothing: \
         {control:?}"
    );
    for protocol in [Protocol::Legacy, Protocol::Repair, Protocol::Credentials] {
        for durable in [Durable::Ahead, Durable::Acknowledged] {
            let control = granted(protocol, durable, Some(mutants::Mutant::CheckedOnce));
            assert!(
                control.answered.is_some() && control.control != 0 && control.records,
                "NEGATIVE CONTROL {} {durable:?}: one check for every grant grants the removed Mac \
                 the rest of the session: {control:?}",
                alpn(protocol)
            );
        }
    }
    assert!(
        later.is_empty(),
        "a Remove made durable after the check left Mac 2 later grants: {later:?}"
    );
}

/// PF4: the acknowledged log a Mac's records custody holds is classified for judging a Commons
/// peer ([`client::CommonsLog`]): trusted where it is of `trust`'s root and carries Commons epochs,
/// another root's under another `Trust`, nothing to judge by with no Commons epoch or no records.
/// A records file of another vault lineage is not read into the store at all
/// (`Store::under`), so another lineage is classified only for a store a caller hands in
/// (`aterm-secrets`' `a_log_of_another_lineage_is_told_from_one_with_nothing_to_judge`).
#[test]
fn the_acknowledged_log_is_classified_for_judging_a_peer() {
    use client::CommonsLog;
    let lab = Lab::new();
    let (founded, _) = founded_then_removed(&lab);
    let kind = |log: CommonsLog| match log {
        CommonsLog::Trusted(..) => "trusted",
        CommonsLog::NoLog => "no log",
        CommonsLog::NoCommonsEpochs => "no commons epochs",
        CommonsLog::OtherRoot => "other root",
        CommonsLog::OtherLineage => "other lineage",
    };
    let held = |name: &str, log: &Log| {
        let home = lab.root.path().join(name);
        aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
        custody(&home, 0, log)
    };
    let paths = held("ours", &founded);
    let judged = client::acknowledged_commons_log(&paths, lab.trust).unwrap();
    assert_eq!(kind(judged), "trusted");
    let mut other = lab.trust;
    other.network[0] ^= 1;
    let judged = client::acknowledged_commons_log(&paths, other).unwrap();
    assert_eq!(kind(judged), "other root");

    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0), entrant(&lab, 1)], 1).unwrap();
    let paths = held("bare", &log_of(std::slice::from_ref(&genesis)));
    let judged = client::acknowledged_commons_log(&paths, lab.trust).unwrap();
    assert_eq!(kind(judged), "no commons epochs");

    let home = lab.root.path().join("empty");
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    let judged =
        client::acknowledged_commons_log(&client::Paths::for_home(&home), lab.trust).unwrap();
    assert_eq!(kind(judged), "no log");
}
