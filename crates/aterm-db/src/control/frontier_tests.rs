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
        Wire::Delta => metadata + 4 + 13 + 32 * View::of(endpoint).have().len(),
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
                Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
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
                listener.accept().unwrap().0,
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
        &View::of(receiver).have(),
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
            let (tcp, _) = Connection::admit(listener.accept().unwrap().0, Scope::Loopback)
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
