// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real sync path decodes received frames once and preserves its receive policy.

#![cfg(unix)]

use aterm_digest::Sha256;
use aterm_secrets::{
    SecretBytes,
    age::Identity,
    b64, client,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Body, Draft, Entrant, EpochAudience, Inserted, Log, MemberId, Record, RecordId, Store,
        decode_records, test_record_decode_attempts,
    },
    text::{Audience, Entry},
};
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const NAME: &str = "decoded-receive";
const WRITER: [u8; 32] = [41; 32];
const INCARNATION: [u8; 16] = [42; 16];
const SIGNER: [u8; 32] = [43; 32];
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const HANG: Duration = Duration::from_secs(60);

type BeforeBody = Box<dyn FnOnce() + Send>;

struct Answer {
    body: Arc<Vec<u8>>,
    before_body: Option<BeforeBody>,
}

struct HostState {
    answer: Answer,
    gets: Vec<String>,
    errors: Vec<String>,
}

struct Host {
    addr: SocketAddr,
    state: Arc<Mutex<HostState>>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Host {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(HostState {
            answer: Answer {
                body: Arc::new(Vec::new()),
                before_body: None,
            },
            gets: Vec::new(),
            errors: Vec::new(),
        }));
        let stopping = Arc::new(AtomicBool::new(false));
        let (serving, stop) = (Arc::clone(&state), Arc::clone(&stopping));
        let worker = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Err(error) = serve(&mut stream, &serving) {
                    serving.lock().unwrap().errors.push(error.to_string());
                }
            }
        });
        Self {
            addr,
            state,
            stopping,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/v1/{NAME}", self.addr)
    }

    fn answer(&self, body: Vec<u8>, before_body: Option<BeforeBody>) {
        self.state.lock().unwrap().answer = Answer {
            body: Arc::new(body),
            before_body,
        };
    }

    fn last_have(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        assert!(
            state.errors.is_empty(),
            "fixture errors: {:?}",
            state.errors
        );
        let target = state
            .gets
            .last()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        target
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|field| field.strip_prefix("have="))
            .unwrap()
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn requests(&self) -> usize {
        self.state.lock().unwrap().gets.len()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(stream: &mut TcpStream, state: &Mutex<HostState>) -> io::Result<()> {
    stream.set_read_timeout(Some(HANG))?;
    stream.set_write_timeout(Some(HANG))?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() == 16 * 1024 {
            return Err(io::Error::other("request header too long"));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = std::str::from_utf8(&head).map_err(io::Error::other)?;
    let request = head.split("\r\n").next().unwrap();
    if !request.starts_with(&format!("GET /v1/{NAME}/records?")) {
        return Err(io::Error::other("unexpected request"));
    }
    let (body, before_body) = {
        let mut state = state.lock().unwrap();
        state.gets.push(request.to_owned());
        (
            Arc::clone(&state.answer.body),
            state.answer.before_body.take(),
        )
    };
    let status = if body.is_empty() {
        "204 No Content"
    } else {
        "200 OK"
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if let Some(change) = before_body {
        change();
    }
    stream.write_all(&body)
}

struct Fixture {
    host: Host,
    paths: client::Paths,
    genesis: Record,
    identity: Identity,
    actor: MemberId,
    _root: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = client::Paths::for_home(&root.path().join("member"));
        let identity = client::machine_identity(&paths).unwrap();
        let request = Request::create(
            [44; 16],
            [45; 32],
            [46; 16],
            INCARNATION,
            &identity,
            &WRITER,
        )
        .unwrap();
        let actor = request.binding().actor();
        let genesis = Record::found(
            &SIGNER,
            vec![Entrant {
                request,
                node_key: [47; 32],
                name: "member".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let host = Host::start();
        client::configure_log_member(
            &paths,
            &format!(
                "{}#{}",
                host.url(),
                b64::encode_padded(&envelope::signer_public(&SIGNER).unwrap())
            ),
            "member",
        )
        .unwrap();
        let config = client::config(&paths).unwrap().unwrap();
        assert_eq!(
            config.signer,
            Some(envelope::signer_public(&SIGNER).unwrap())
        );
        assert!(!client::alab_vault(&config));
        assert_eq!(client::save_records(&paths, genesis.bytes()).unwrap(), 1);
        let fixture = Self {
            host,
            paths,
            genesis,
            identity,
            actor,
            _root: root,
        };
        fixture.assert_records(&[&fixture.genesis]);
        fixture.warm();
        fixture
    }

    fn warm(&self) {
        let first = client::load_records(&self.paths).unwrap();
        let second = client::load_records(&self.paths).unwrap();
        assert_eq!(first.inventory(), second.inventory());
    }

    fn sync(&self) -> aterm_secrets::Result<usize> {
        let before = self.host.requests();
        let result = client::test_sync_records_without_discovery(&self.paths);
        assert_eq!(self.host.requests(), before + 1);
        self.host.last_have();
        result
    }

    fn name(
        &self,
        name: &str,
        at: u64,
        sequence: u64,
        previous: Option<RecordId>,
        parents: Vec<RecordId>,
    ) -> Record {
        Draft {
            network: *self.genesis.id(),
            sequence,
            previous,
            parents,
            authored_ms: at,
            body: Body::Name {
                name: name.to_owned(),
            },
        }
        .sign(&WRITER, INCARNATION)
        .unwrap()
    }

    fn root_name(&self, name: &str, at: u64) -> Record {
        self.name(name, at, 1, None, vec![*self.genesis.id()])
    }

    fn assert_records(&self, records: &[&Record]) -> Store {
        let expected = wire(records);
        assert!(
            std::fs::read(self.paths.records_file()).unwrap() == expected,
            "exact canonical signed bytes changed"
        );
        let store = client::load_records(&self.paths).unwrap();
        let ids: BTreeSet<_> = records.iter().map(|record| *record.id()).collect();
        assert_eq!(store.inventory(), ids);
        assert_eq!(store.complete_inventory(), ids);
        for record in records {
            assert_eq!(store.get_bytes(record.id()), Some(record.bytes()));
        }
        store
    }
}

fn wire(records: &[&Record]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend_from_slice(record.bytes());
    }
    bytes
}

fn hex(id: &RecordId) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in id {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

fn valid_work() -> (usize, usize) {
    let fixture = Fixture::new();
    let mut log = Log::new(fixture.genesis.clone()).unwrap();
    let mut records = vec![fixture.genesis.clone()];
    let (draft, key) = log
        .draft_epoch(&fixture.actor, 2, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&WRITER, INCARNATION).unwrap();
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch.clone()).unwrap(), Inserted::Held);
    records.push(epoch);
    let wrap = log
        .draft_wrap(
            &fixture.actor,
            3,
            epoch_id,
            &key,
            fixture.identity.recipient(),
        )
        .unwrap()
        .sign(&WRITER, INCARNATION)
        .unwrap();
    assert_eq!(log.insert(wrap.clone()).unwrap(), Inserted::Held);
    records.push(wrap);
    for i in 0..VALUES {
        let entry = Entry {
            name: format!("value-{i}"),
            value: SecretBytes::new(vec![i as u8; VALUE_BYTES]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let put = log
            .draft_put(&fixture.actor, 4 + i as u64, epoch_id, &key, &entry)
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        assert_eq!(log.insert(put.clone()).unwrap(), Inserted::Held);
        records.push(put);
    }
    let original: Vec<_> = records.iter().collect();
    let mut offered = original.clone();
    offered.push(records.last().unwrap());
    offered.push(&fixture.genesis);
    let bytes = wire(&offered);
    let count = offered.len();
    assert_eq!(count, VALUES + 5);
    assert_eq!(decode_records(&bytes).unwrap().len(), count);
    fixture.host.answer(bytes, None);
    fixture.warm();
    let before = test_record_decode_attempts();
    let answer = fixture.sync();
    let attempts = test_record_decode_attempts() - before;
    // End the observation before fresh cache reads or independent decryption.
    assert_eq!(answer.unwrap(), records.len() - 1);
    let store = fixture.assert_records(&original);
    let entries = store
        .log()
        .unwrap()
        .open_entries(&fixture.identity)
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), VALUES);
    for i in 0..VALUES {
        let entry = entries
            .iter()
            .find(|entry| entry.name == format!("value-{i}"))
            .unwrap();
        assert_eq!(entry.value.len(), VALUE_BYTES);
        assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
    }
    (attempts, count)
}

fn unreadable_descendants(unknown_kind: bool) {
    let fixture = Fixture::new();
    let hidden = fixture.root_name("unreadable", 10);
    let mut bad = hidden.bytes().to_vec();
    if unknown_kind {
        // ALM1's kind follows its four-byte magic, version, and network digest.
        assert_eq!(&bad[..5], b"ALM1\x01");
        assert_eq!(bad[4 + 1 + 32], 4, "the original is a Name");
        bad[4 + 1 + 32] = 0xfe;
    } else {
        *bad.last_mut().unwrap() ^= 1;
    }
    assert!(Record::decode(&bad).is_err());
    let bad_id = Sha256::digest(&bad);
    let child = fixture.name(
        "child",
        11,
        2,
        Some(bad_id),
        vec![bad_id, *fixture.genesis.id()],
    );
    let grandchild = fixture.name(
        "grandchild",
        12,
        3,
        Some(*child.id()),
        vec![*child.id(), *fixture.genesis.id()],
    );
    let good = fixture.root_name("good", 20);
    let later = fixture.name(
        "later",
        21,
        2,
        Some(*good.id()),
        vec![*good.id(), *fixture.genesis.id()],
    );
    // Descendants precede the unreadable parent; both readable siblings survive.
    let mut response = wire(&[&grandchild, &later]);
    response.extend_from_slice(&bad);
    response.extend_from_slice(&wire(&[&child, &good]));
    fixture.host.answer(response, None);
    assert_eq!(fixture.sync().unwrap(), 2);
    let store = fixture.assert_records(&[&fixture.genesis, &good, &later]);
    for id in [&bad_id, child.id(), grandchild.id()] {
        assert!(
            !store.contains(id),
            "unreadable dependency entered even waiting inventory"
        );
    }
    fixture.host.answer(Vec::new(), None);
    assert_eq!(fixture.sync().unwrap(), 0);
    assert_eq!(
        fixture.host.last_have().first(),
        Some(&hex(grandchild.id())),
        "next pull preserves the unreadable leaf, not only its unknown ancestor"
    );
    fixture.assert_records(&[&fixture.genesis, &good, &later]);
}

fn malformed_tail_keeps_no_prefix() {
    let fixture = Fixture::new();
    let good = fixture.root_name("after-malformed", 10);
    let mut malformed = good.bytes().to_vec();
    malformed.extend_from_slice(b"ALM");
    fixture.host.answer(malformed, None);
    assert!(fixture.sync().is_err());
    fixture.assert_records(&[&fixture.genesis]);
    fixture.host.answer(good.bytes().to_vec(), None);
    assert_eq!(fixture.sync().unwrap(), 1);
    assert_eq!(
        fixture.host.last_have().first(),
        Some(&hex(fixture.genesis.id()))
    );
    fixture.assert_records(&[&fixture.genesis, &good]);
}

fn final_store_preserves_concurrent_addition() {
    let fixture = Fixture::new();
    let incoming = fixture.root_name("incoming", 10);
    let concurrent = fixture.root_name("concurrent", 11);
    let path = fixture.paths.records_file();
    let newer = wire(&[&fixture.genesis, &concurrent]);
    fixture.host.answer(
        incoming.bytes().to_vec(),
        Some(Box::new(move || {
            fsx::write_atomic(&path, &newer, 0o600).unwrap();
        })),
    );
    assert_eq!(fixture.sync().unwrap(), 1);
    fixture.assert_records(&[&fixture.genesis, &concurrent, &incoming]);
}

fn final_store_reconsiders_an_initially_known_record() {
    let fixture = Fixture::new();
    let known = fixture.root_name("known-before-fetch", 10);
    assert_eq!(
        client::save_records(&fixture.paths, known.bytes()).unwrap(),
        1
    );
    fixture.warm();
    let path = fixture.paths.records_file();
    let older = fixture.genesis.bytes().to_vec();
    fixture.host.answer(
        known.bytes().to_vec(),
        Some(Box::new(move || {
            fsx::write_atomic(&path, &older, 0o600).unwrap();
        })),
    );
    assert_eq!(fixture.sync().unwrap(), 1);
    assert_eq!(fixture.host.last_have().first(), Some(&hex(known.id())));
    fixture.assert_records(&[&fixture.genesis, &known]);
}

#[test]
fn actual_sync_decodes_once_and_keeps_receive_semantics() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = client::ServersAnswering::within(HANG);
    let (attempts, frames) = valid_work();
    unreadable_descendants(true);
    unreadable_descendants(false);
    malformed_tail_keeps_no_prefix();
    final_store_preserves_concurrent_addition();
    final_store_reconsiders_an_initially_known_record();
    println!("actual receive frames={frames} decode_attempts={attempts} bound={frames}");
    assert!(
        attempts > 0,
        "novel signed records required actual decoding"
    );
    assert!(
        attempts <= frames,
        "the real sync decoded frames repeatedly: {attempts} > {frames}"
    );
}
