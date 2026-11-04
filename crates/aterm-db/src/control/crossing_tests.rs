// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::Paths;
use aterm_secrets::membership_log::{Record, RecordBudgets};

fn custody(lab: &Lab, name: &str, crossed: bool) -> Paths {
    let paths = Paths::for_home(&lab.root.path().join(name));
    std::fs::create_dir_all(&paths.dir).unwrap();
    if crossed {
        // The durable mark counts even after the records file is absent or short.
        std::fs::write(paths.records_file().with_extension("crossed"), b"crossed\n").unwrap();
    }
    paths
}

#[test]
fn crossed_custody_cannot_complete_native_legacy_database_tls() {
    let lab = Lab::new();
    for marked in [0, 1] {
        let mut endpoints = [lab.endpoint(0), lab.endpoint(1)];
        endpoints[marked].paths = Some(custody(&lab, &format!("tls-{marked}"), true));
        for version_two in [false, true] {
            endpoints[0].legacy_projection = version_two;
            endpoints[1].legacy_projection = version_two;
            endpoints[0].legacy_credentials = !version_two;
            endpoints[1].legacy_credentials = !version_two;
            let (server, client, _) = dial(&endpoints[0], &endpoints[1]);
            assert!(server.is_err(), "marked {marked}, v2 {version_two}");
            assert!(client.is_err(), "marked {marked}, v2 {version_two}");
        }
    }
}

#[test]
fn a_responder_rechecks_crossing_after_admission_before_its_hello() {
    let lab = Lab::new();
    for cross_after_admission in [false, true] {
        let paths = custody(&lab, &format!("late-{cross_after_admission}"), false);
        let mark = paths.records_file().with_extension("crossed");
        let endpoint = lab.endpoint(1).with_membership_paths(paths);
        let peer = lab.endpoint(0);
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
        let _ = observe_peer_cache(
            move |stage| {
                if cross_after_admission
                    && matches!(stage, PeerCacheStage::Acquired(Side::Responder))
                {
                    std::fs::write(&mark, b"crossed\n").unwrap();
                }
            },
            || endpoint.connected(&mut stream, Some(&proof), Side::Responder, &mut repair),
        );
        assert_eq!(stream.output.is_empty(), cross_after_admission);
    }
}

#[test]
fn cached_membership_disk_stops_serving_after_the_durable_mark() {
    use aterm_secrets::membership_log::Entrant;

    let lab = Lab::new();
    let paths = custody(&lab, "cached", false);
    let genesis = Record::found(
        &[7; 32],
        vec![Entrant {
            request: lab.requests[0].clone(),
            node_key: *lab.endpoint(0).identity.key().bytes(),
            name: "fixture".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    aterm_secrets::client::save_records(&paths, genesis.bytes()).unwrap();
    let disk = MembershipDisk::new(&paths, lab.trust);
    let before = disk.inventory().unwrap();
    assert!(before.contains(genesis.id()));
    assert_eq!(disk.batch(&[*genesis.id()]).unwrap().len(), 1);
    std::fs::write(paths.records_file().with_extension("crossed"), b"crossed\n").unwrap();
    assert!(disk.inventory().is_err());
    assert!(disk.batch(&[*genesis.id()]).is_err());
    assert!(
        disk.select_missing(&before, &BTreeSet::new(), &mut None)
            .is_err()
    );
    assert!(disk.retain(&[genesis.bytes().to_vec()]).is_err());
}

/// The log a Mac's records file holds in the legacy database fence's conformance, rooted in the
/// lab's trust: F's genesis and `Name`s up to three records short of [`LEGACY_RECORDS`] (the
/// base), and five more of F's `Name`s past them (the batch), standing for
/// `SwarmRecordsCapacity`'s removal batch. Neither the fence nor the exchange reads a record's
/// kind, and none of these seats or removes the peer, which a Mac judges before its hello.
///
/// [`LEGACY_RECORDS`]: aterm_secrets::membership_log::LEGACY_RECORDS
fn capacity_log(trust: Trust) -> (Vec<Record>, Vec<Record>) {
    use aterm_secrets::membership_log::{Body, Entrant, LEGACY_RECORDS, Log};
    let writer = [0x61; 32];
    let incarnation = [0x61; 16];
    let request = Request::create(
        trust.network,
        trust.authority,
        [0x81; 16],
        incarnation,
        &Identity::from_secret_bytes([0xa1; 32]),
        &writer,
    )
    .unwrap();
    let member = request.binding().actor();
    let genesis = Record::found(
        &[7; 32],
        vec![Entrant {
            request,
            node_key: [0xe1; 32],
            name: "mac-f".into(),
            label: "m1".into(),
        }],
        1_000,
    )
    .unwrap();
    raised().within(|| {
        let mut log = Log::new(genesis.clone()).unwrap();
        let mut records = vec![genesis];
        while records.len() < LEGACY_RECORDS + 2 {
            let at = 1_000 + u64::try_from(records.len()).unwrap();
            let name = log
                .draft(
                    &member,
                    at,
                    Body::Name {
                        name: format!("filler-{at}"),
                    },
                )
                .sign(&writer, incarnation)
                .unwrap();
            let _ = log.insert(name.clone()).unwrap();
            records.push(name);
        }
        let batch = records.split_off(LEGACY_RECORDS - 3);
        (records, batch)
    })
}

/// A test held bound past [`LEGACY_RECORDS`] that holds the whole capacity fixture.
///
/// [`LEGACY_RECORDS`]: aterm_secrets::membership_log::LEGACY_RECORDS
fn raised() -> RecordBudgets {
    RecordBudgets {
        held: 2 * aterm_secrets::membership_log::LEGACY_RECORDS,
        ..RecordBudgets::PRODUCTION
    }
}

/// The budgets a Mac holds `model`'s `Mac` scenario within: an explicit legacy held bound where
/// its total `CX` past the base is `LX`, and production capacity where it takes the whole batch.
fn capacity_budgets(model: &aterm_spec::derive::Model, base: usize, batch: usize) -> RecordBudgets {
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(constant, _)| *constant == name)
            .map(|&(_, value)| usize::try_from(value).unwrap())
            .unwrap()
    };
    assert_eq!(constant("R"), batch);
    assert_eq!(
        base + constant("LX"),
        aterm_secrets::membership_log::LEGACY_RECORDS
    );
    if constant("CX") == constant("LX") {
        RecordBudgets {
            held: aterm_secrets::membership_log::LEGACY_RECORDS,
            ..RecordBudgets::PRODUCTION
        }
    } else {
        assert_eq!(constant("CX"), batch);
        RecordBudgets::PRODUCTION
    }
}

/// What a stream wrote, kept beside it.
struct Tee<S> {
    stream: S,
    written: Vec<u8>,
}
impl<S: Read> Read for Tee<S> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(bytes)
    }
}
impl<S: Write> Write for Tee<S> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.stream.write(bytes)?;
        self.written.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

/// A Mac's custody holding `state`'s durable state: its records file the base, then the batch's
/// first `nr` records taken onto it as a Mac takes them (the mark first where the file passes
/// the legacy bound), and its crossed mark stored where `mark` is 1. Returns the custody and the
/// count its log holds.
fn realize(
    lab: &Lab,
    name: &str,
    state: &aterm_spec::interp::State,
    (base, batch): (&[Record], &[Record]),
) -> (Paths, usize) {
    let paths = custody(lab, name, state["mark"] == 1);
    let nr = usize::try_from(state["nr"]).unwrap();
    for records in [base, &batch[..nr]] {
        aterm_secrets::client::save_records(
            &paths,
            &aterm_secrets::membership_log::encode_records(records),
        )
        .unwrap();
    }
    let on_file = held(&paths, batch);
    assert_eq!(on_file, (base.len() + nr, nr), "{name}: the file");
    assert_eq!(
        aterm_secrets::client::records_marked_crossed(&paths).unwrap(),
        state["mark"] == 1,
        "{name}: the mark"
    );
    (paths, on_file.0)
}

/// How many records `paths`' file holds in all, and how many of `batch`.
fn held(paths: &Paths, batch: &[Record]) -> (usize, usize) {
    let store = aterm_secrets::client::load_records(paths).unwrap();
    let all = store
        .log()
        .map_or(0, aterm_secrets::membership_log::Log::held_count);
    let of_batch = batch
        .iter()
        .filter(|record| store.contains(record.id()))
        .count();
    (all, of_batch)
}

/// One exchange between a Mac (`mac`, on this thread, on `side`) and a database peer of
/// `protocol` (`atdb/2` or `atdb/3`) whose custody holds the base, as every member's does (the
/// records file of `peer_base`, copied to a home of its own): whether the Mac refused it before
/// its hello (it wrote nothing at all), and how many of `batch`'s records it served.
fn legacy_exchange(
    lab: &Lab,
    mac: &Endpoint,
    (peer_base, home): (&Paths, &str),
    batch: &[Record],
    protocol: Protocol,
    side: Side,
) -> (bool, usize) {
    let peer_paths = Paths::for_home(&lab.root.path().join(home));
    std::fs::create_dir_all(&peer_paths.dir).unwrap();
    std::fs::copy(peer_base.records_file(), peer_paths.records_file()).unwrap();
    let peer = lab.endpoint(0).with_membership_paths(peer_paths);
    let proof = |endpoint: &Endpoint| NodeProof {
        node: endpoint.identity.node().into(),
        key: endpoint.identity.key(),
    };
    let (of_peer, of_mac) = (proof(&peer), proof(mac));
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    for end in [&ours, &theirs] {
        // A hang detector: each side's next frame must come.
        end.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
    }
    let peer_side = match side {
        Side::Initiator => Side::Responder,
        Side::Responder => Side::Initiator,
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let mut theirs = theirs;
            let _ = peer.connected_until(
                &mut theirs,
                Some(&of_mac),
                peer_side,
                &mut Repair::default(),
                Instant::now() + EXCHANGE,
                protocol,
            );
        });
        let mut stream = Tee {
            stream: ours,
            written: Vec::new(),
        };
        let _ = mac.connected_until(
            &mut stream,
            Some(&of_peer),
            side,
            &mut Repair::default(),
            Instant::now() + EXCHANGE,
            protocol,
        );
        let Tee { stream, written } = stream;
        // The peer reads the end of the Mac's answer, whatever it was.
        drop(stream);
        answered.join().unwrap();
        let served = batch
            .iter()
            .filter(|record| {
                written
                    .windows(record.bytes().len())
                    .any(|window| window == record.bytes())
            })
            .count();
        (written.is_empty(), served)
    })
}

/// What a legacy database peer is answered by a Mac whose durable state is `state`'s
/// ([`realize`]), its endpoint stating the bound its log's count does, as an endpoint made from
/// that log states it (`RecordsBound::here` of `past_legacy`): whether it refused the peer
/// before its hello, and how many of the batch's records it served. Every protocol of `atdb/2`
/// and `atdb/3`, from either side, must agree, and none changes the Mac's file or mark.
fn hello_answer(
    lab: &Lab,
    name: &str,
    state: &aterm_spec::interp::State,
    log: (&[Record], &[Record]),
    peer_base: &Paths,
) -> (bool, usize) {
    use aterm_secrets::crossing::{RecordsBound, past_legacy};
    let (paths, count) = realize(lab, name, state, log);
    let mut answers = BTreeSet::new();
    let mut peers = 0;
    for protocol in [Protocol::Legacy, Protocol::Repair, Protocol::Credentials] {
        for side in [Side::Initiator, Side::Responder] {
            let mut mac = lab.endpoint(1).with_membership_paths(paths.clone());
            mac.records_bound = RecordsBound::here(past_legacy(count));
            peers += 1;
            answers.insert(legacy_exchange(
                lab,
                &mac,
                (peer_base, &format!("{name}-peer-{peers}")),
                log.1,
                protocol,
                side,
            ));
        }
    }
    assert_eq!(
        answers.len(),
        1,
        "{name}: every protocol and side alike: {answers:?}"
    );
    let nr = usize::try_from(state["nr"]).unwrap();
    assert_eq!(held(&paths, log.1), (count, nr), "{name}: the file after");
    assert_eq!(
        aterm_secrets::client::records_marked_crossed(&paths).unwrap(),
        state["mark"] == 1,
        "{name}: the mark after"
    );
    answers.into_iter().next().unwrap()
}

/// `SwarmRecordsCapacity`'s variables after a legacy database peer's exchange with a Mac in
/// `from`: `fenced` whether the Mac refused it before its hello (it wrote nothing at all), and
/// `gs` how many of the removal batch's records it served. The exchange leaves the Mac's file
/// and mark as they were, the rest of `from`.
fn records_capacity_hello(
    from: &aterm_spec::interp::State,
    refused: bool,
    served: usize,
) -> aterm_spec::interp::State {
    let mut next = from.clone();
    next.insert("fenced", i64::from(refused));
    next.insert("gs", i64::try_from(served).unwrap());
    next
}

/// S8: a Mac's legacy database fence refines `SwarmRecordsCapacity`'s `LegacyDbHello` in each
/// configuration the model declares: from every state of its `Mac` scenario the model reaches,
/// realized as a records file and a mark ([`realize`]), a peer of `atdb/2` or `atdb/3` that
/// holds the base, on every protocol and from either side, gives exactly the model's successor:
/// the Mac refuses it before its hello, or serves it every record of the batch it holds.
///
/// NEGATIVE CONTROL: with `MARK_UNREAD` (the fence reads no durable mark, only the count the
/// Mac holds) a Mac that pulled a crossed log's records within the legacy bound sends a legacy
/// peer its hello and those records, which is no successor of `LegacyDbHello` and breaks
/// `LegacyNeverPartial`.
#[test]
fn the_legacy_database_fence_conforms_to_the_capacity_model() {
    let name = aterm_models_swarm::records_capacity_model().name;
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configurations| configurations[0].name == name)
        .expect("SwarmRecordsCapacity is registered");
    let lab = Lab::new();
    for n in 0..2 {
        lab.journal(n).ingest(lab.genesis.bytes()).unwrap();
    }
    let (base, batch) = capacity_log(lab.trust);
    let log = (base.as_slice(), batch.as_slice());
    let peer_base = custody(&lab, "peer-base", false);
    aterm_secrets::client::save_records(
        &peer_base,
        &aterm_secrets::membership_log::encode_records(&base),
    )
    .unwrap();
    let mut judged = 0;
    let mut served_any = false;
    for (c, model) in configurations.iter().enumerate() {
        let budgets = capacity_budgets(model, base.len(), batch.len());
        let mut answers: std::collections::BTreeMap<(i64, i64), (bool, usize)> =
            std::collections::BTreeMap::new();
        budgets.within(|| {
            for state in aterm_spec::interp::reachable_states(model) {
                if state["sc"] != 3 {
                    continue;
                }
                assert!(model.action_enabled("LegacyDbHello", &state));
                let key = (state["nr"], state["mark"]);
                let (refused, served) = *answers.entry(key).or_insert_with(|| {
                    hello_answer(
                        &lab,
                        &format!("c{c}-nr{}-mark{}", key.0, key.1),
                        &state,
                        log,
                        &peer_base,
                    )
                });
                served_any |= served > 0;
                assert_eq!(
                    model.successors("LegacyDbHello", &state),
                    vec![records_capacity_hello(&state, refused, served)],
                    "LegacyDbHello from {state:?}"
                );
                judged += 1;
            }
        });
    }
    assert!(judged > 0);
    assert!(served_any, "some Mac served the batch's records it holds");

    let committed = &configurations[0];
    let pulled = ["Mac", "MacPulls"]
        .iter()
        .fold(committed.init_state(), |state, action| {
            committed.successors(action, &state).remove(0)
        });
    assert_eq!((pulled["nr"], pulled["nx"], pulled["mark"]), (3, 1, 1));
    MARK_UNREAD.with(|cell| cell.set(true));
    let (refused, served) = hello_answer(&lab, "unread", &pulled, log, &peer_base);
    MARK_UNREAD.with(|cell| cell.set(false));
    assert_eq!(
        (refused, served),
        (false, 3),
        "mutant: the hello and the records go out"
    );
    let observed = records_capacity_hello(&pulled, refused, served);
    assert!(
        !committed
            .successors("LegacyDbHello", &pulled)
            .contains(&observed)
    );
    assert!(!committed.check_invariant("LegacyNeverPartial", &observed));
}

/// The inventory wire carries no log past the legacy count: an inventory of 4,097 ids is refused
/// by its sender, and the frame a sender without that check would write fails the reader of a
/// build on `atdb/3` or older, which takes at most `journal::MAX_NODES` ids. `atdb/4` and `atdb/5`
/// send no inventory, only frontiers and ancestry checkpoints.
///
/// NEGATIVE CONTROL (in the test): an inventory of 4,096 ids passes both.
#[test]
fn an_inventory_past_the_legacy_count_fails_a_legacy_peer() {
    assert_eq!(MAX_NODES, aterm_secrets::membership_log::LEGACY_RECORDS);
    let inventory = |n: usize| -> BTreeSet<Digest> {
        (0..u32::try_from(n).unwrap())
            .map(|i| {
                let mut id = [0; 32];
                id[..4].copy_from_slice(&i.to_be_bytes());
                id
            })
            .collect()
    };
    for (n, fits) in [(MAX_NODES, true), (MAX_NODES + 1, false)] {
        let ids = inventory(n);
        assert_eq!(send_inventory(&mut Vec::new(), &ids).is_ok(), fits);
        let mut frame = u32::try_from(ids.len()).unwrap().to_le_bytes().to_vec();
        for id in &ids {
            frame.extend_from_slice(id);
        }
        assert_eq!(receive_inventory(&mut frame.as_slice()).is_ok(), fits);
    }
}
