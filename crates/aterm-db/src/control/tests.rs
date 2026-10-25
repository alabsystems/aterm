// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{age::Identity, commons::node::Edit};
use std::{io::Cursor, net::TcpListener, thread};

const ROOT: [u8; 32] = [17; 32];
const NETWORK: [u8; 16] = [9; 16];

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
                eb.exchange(listener.accept().unwrap().0, Side::Responder, b)
                    .unwrap()
            });
            let report = ea
                .exchange(TcpStream::connect(addr).unwrap(), Side::Initiator, a)
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
                listener.accept().unwrap().0,
                Side::Responder,
                &mut Repair::default(),
            )
        });
        assert!(
            impostor
                .exchange(
                    TcpStream::connect(addr).unwrap(),
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
