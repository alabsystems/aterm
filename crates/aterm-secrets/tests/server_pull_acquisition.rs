// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real periodic peer-pull acquisition preserves known owners, commits and memo policy.

#![cfg(unix)]

use aterm_digest::Sha256;
use aterm_secrets::{
    SecretBytes,
    age::Identity,
    commons::binding::Request,
    discover::DiscoveredVault,
    envelope, fsx,
    membership_log::{
        Body, Entrant, EpochAudience, Inserted, Log, MAX_RECORD, MemberId, Record, RecordId, Store,
        test_record_decode_attempts,
    },
    server::{self, Running, VaultTxt},
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static GROSS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    // Only our peer's bounded HTTP worker opts out; every actual pull worker counts.
    static FIXTURE_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn allocated(bytes: usize) {
    if ACTIVE.load(Ordering::SeqCst) && !FIXTURE_THREAD.try_with(Cell::get).unwrap_or(false) {
        GROSS.fetch_add(bytes, Ordering::SeqCst);
    }
}

struct Counting;

// SAFETY: each allocation is forwarded unchanged to System. Constant TLS and
// atomics allocate nothing. Gross counts successful requested sizes, including
// realloc's new size; old cache/worker ownership transfers need no subtraction.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies the valid allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies the valid allocation layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forward the original live allocation and requested size.
        let pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !pointer.is_null() {
            allocated(new_size);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forward the allocation and its matching layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measuring;
impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::SeqCst);
    }
}

const NAME: &str = "server-pull-acquisition";
const WRITER: [u8; 32] = [71; 32];
const INCARNATION: [u8; 16] = [72; 16];
const SIGNER: [u8; 32] = [73; 32];
const HANG: Duration = Duration::from_secs(60);
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;

#[derive(Clone)]
struct Frame {
    id: RecordId,
    parents: Vec<RecordId>,
    bytes: Arc<[u8]>,
}

impl Frame {
    fn of(record: &Record) -> Self {
        Self {
            id: *record.id(),
            parents: record.parents().to_vec(),
            bytes: Arc::from(record.bytes()),
        }
    }
    fn raw(bytes: Vec<u8>, parents: Vec<RecordId>) -> Self {
        Self {
            id: Sha256::digest(&bytes),
            parents,
            bytes: Arc::from(bytes),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Framing {
    Length,
    Chunked,
    ShortLength,
    BadChunk,
}

type BeforeBody = Box<dyn FnOnce() + Send>;

struct Offer {
    frames: Vec<Frame>,
    honor_have: bool,
    framing: Framing,
    before_body: Option<BeforeBody>,
}

#[derive(Clone)]
struct RequestSeen {
    records: bool,
    have: Vec<String>,
    ids: Vec<RecordId>,
    bytes: usize,
    finished: bool,
}

struct PeerState {
    offer: Offer,
    requests: Vec<RequestSeen>,
    errors: Vec<String>,
}

struct Peer {
    addr: SocketAddr,
    state: Arc<Mutex<PeerState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    done: Receiver<()>,
    completed: Cell<usize>,
}

impl Peer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(PeerState {
            offer: Offer {
                frames: Vec::new(),
                honor_have: true,
                framing: Framing::Length,
                before_body: None,
            },
            requests: Vec::new(),
            errors: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (done_tx, done) = mpsc::channel();
        let (serving, stopping) = (Arc::clone(&state), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            FIXTURE_THREAD.with(|excluded| excluded.set(true));
            while let Ok((mut stream, _)) = listener.accept() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Err(error) = serve(&mut stream, &serving) {
                    serving.lock().unwrap().errors.push(error.to_string());
                }
                let _ = done_tx.send(());
            }
        });
        Self {
            addr,
            state,
            stop,
            worker: Some(worker),
            done,
            completed: Cell::new(0),
        }
    }

    fn discovered(&self) -> DiscoveredVault {
        DiscoveredVault {
            url: format!("http://{}/v1/{NAME}", self.addr),
            txt: Some(VaultTxt {
                vault: NAME.into(),
                signer: envelope::fingerprint(&envelope::signer_public(&SIGNER).unwrap()),
                seq: 1,
                head: [0; 32],
                label: Some("m9".into()),
            }),
        }
    }

    fn offer(
        &self,
        frames: Vec<Frame>,
        honor_have: bool,
        framing: Framing,
        before_body: Option<BeforeBody>,
    ) {
        self.traffic();
        let mut state = self.state.lock().unwrap();
        state.offer = Offer {
            frames,
            honor_have,
            framing,
            before_body,
        };
        state.requests.clear();
        state.errors.clear();
        self.completed.set(0);
    }

    fn traffic(&self) -> Vec<RequestSeen> {
        let expected = self.state.lock().unwrap().requests.len();
        while self.completed.get() < expected {
            self.done
                .recv_timeout(HANG)
                .expect("peer response must finish");
            self.completed.set(self.completed.get() + 1);
        }
        let state = self.state.lock().unwrap();
        assert!(state.errors.is_empty(), "peer errors: {:?}", state.errors);
        state.requests.clone()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn serve(stream: &mut TcpStream, state: &Mutex<PeerState>) -> io::Result<()> {
    stream.set_read_timeout(Some(HANG))?;
    stream.set_write_timeout(Some(HANG))?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16 * 1024 {
            return Err(invalid("request head too large"));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).map_err(io::Error::other)?;
    let line = header.split("\r\n").next().unwrap();
    let target = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| invalid("request target absent"))?;
    if !line.starts_with("GET ") || !target.starts_with(&format!("/v1/{NAME}")) {
        return Err(invalid("unexpected peer request"));
    }
    let records = target.starts_with(&format!("/v1/{NAME}/records?"));
    let have: Vec<String> = if records {
        target
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|part| part.strip_prefix("have="))
            .ok_or_else(|| invalid("record request missing have"))?
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    let (index, frames, framing, before) = {
        let mut state = state.lock().unwrap();
        let selected = if records {
            select(&state.offer.frames, &have, state.offer.honor_have)
        } else {
            Vec::new()
        };
        let bytes = selected.iter().map(|frame| frame.bytes.len()).sum();
        let index = state.requests.len();
        state.requests.push(RequestSeen {
            records,
            have,
            ids: selected.iter().map(|f| f.id).collect(),
            bytes,
            finished: false,
        });
        let before = if records {
            state.offer.before_body.take()
        } else {
            None
        };
        (index, selected, state.offer.framing, before)
    };
    if let Some(before) = before {
        before();
    }
    if !records {
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
    } else {
        write_frames(stream, &frames, framing)?;
    }
    state.lock().unwrap().requests[index].finished = true;
    Ok(())
}

// The fixture's own graph walk honors have without using production framing or
// readable-subset helpers. Bytes/parents are captured independently when signed.
fn select(frames: &[Frame], have: &[String], honor: bool) -> Vec<Frame> {
    if !honor {
        return frames.to_vec();
    }
    let by_id: BTreeMap<_, _> = frames
        .iter()
        .map(|frame| (frame.id, &frame.parents))
        .collect();
    let mut known = BTreeSet::new();
    let mut stack: Vec<_> = frames
        .iter()
        .filter(|frame| have.contains(&hex(&frame.id)))
        .map(|frame| frame.id)
        .collect();
    while let Some(id) = stack.pop() {
        if known.insert(id)
            && let Some(parents) = by_id.get(&id)
        {
            stack.extend(parents.iter().copied());
        }
    }
    frames
        .iter()
        .filter(|frame| !known.contains(&frame.id))
        .cloned()
        .collect()
}

fn write_frames(stream: &mut TcpStream, frames: &[Frame], framing: Framing) -> io::Result<()> {
    let length: usize = frames.iter().map(|frame| frame.bytes.len()).sum();
    if length == 0 {
        return stream.write_all(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    }
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
    )?;
    match framing {
        Framing::Length | Framing::ShortLength => {
            let declared = length + usize::from(matches!(framing, Framing::ShortLength)) * 19;
            write!(stream, "Content-Length: {declared}\r\n\r\n")?;
            for frame in frames {
                for bytes in frame.bytes.chunks(32 * 1024) {
                    stream.write_all(bytes)?;
                }
            }
        }
        Framing::Chunked | Framing::BadChunk => {
            stream.write_all(b"Transfer-Encoding: chunked\r\n\r\n")?;
            for frame in frames {
                for bytes in frame.bytes.chunks(32 * 1024 + 7) {
                    write!(stream, "{:x}\r\n", bytes.len())?;
                    stream.write_all(bytes)?;
                    stream.write_all(b"\r\n")?;
                }
            }
            stream.write_all(if matches!(framing, Framing::BadChunk) {
                b"not-hex\r\n"
            } else {
                b"0\r\n\r\n"
            })?;
        }
    }
    Ok(())
}

struct History {
    identity: Identity,
    actor: MemberId,
    log: Log,
    records: Vec<Record>,
}

impl History {
    fn new(values: usize) -> Self {
        let identity = Identity::from_secret_bytes([74; 32]);
        let request = Request::create(
            [75; 16],
            [76; 32],
            [77; 16],
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
                node_key: [78; 32],
                name: "member".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let mut result = Self {
            identity,
            actor,
            log: Log::new(genesis.clone()).unwrap(),
            records: vec![genesis],
        };
        if values > 0 {
            let (draft, key) = result
                .log
                .draft_epoch(&actor, 2, EpochAudience::Everyone, 1)
                .unwrap();
            let epoch = draft.sign(&WRITER, INCARNATION).unwrap();
            let epoch_id = *epoch.id();
            result.keep(epoch);
            let wrap = result
                .log
                .draft_wrap(&actor, 3, epoch_id, &key, result.identity.recipient())
                .unwrap()
                .sign(&WRITER, INCARNATION)
                .unwrap();
            result.keep(wrap);
            for i in 0..values {
                let entry = Entry {
                    name: format!("value-{i}"),
                    value: SecretBytes::new(vec![i as u8; VALUE_BYTES]),
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                };
                let put = result
                    .log
                    .draft_put(&actor, 4 + i as u64, epoch_id, &key, &entry)
                    .unwrap()
                    .sign(&WRITER, INCARNATION)
                    .unwrap();
                result.keep(put);
            }
            let entries = result.log.open_entries(&result.identity).unwrap().unwrap();
            assert_eq!(entries.len(), values);
            for i in 0..values {
                let entry = entries
                    .iter()
                    .find(|entry| entry.name == format!("value-{i}"))
                    .unwrap();
                assert_eq!(entry.value.len(), VALUE_BYTES);
                assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
            }
        }
        result
    }

    fn keep(&mut self, record: Record) {
        assert_eq!(self.log.insert(record.clone()).unwrap(), Inserted::Held);
        self.records.push(record);
    }

    fn mint(&mut self, name: &str) -> Record {
        let record = self
            .log
            .draft(
                &self.actor,
                100 + self.records.len() as u64,
                Body::Name { name: name.into() },
            )
            .sign(&WRITER, INCARNATION)
            .unwrap();
        self.keep(record.clone());
        record
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    bytes: Vec<u8>,
    inventory: BTreeSet<RecordId>,
    complete: BTreeSet<RecordId>,
    heads: Vec<RecordId>,
    on_file: Option<usize>,
    stamp: fsx::FileStamp,
}

struct Fixture {
    running: Option<Running>,
    peer: Peer,
    path: PathBuf,
    _root: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new(initial: &[Record]) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join(format!("{NAME}.records"));
        let bytes = wire(initial.iter());
        fsx::write_atomic(&path, &bytes, 0o600).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let options = server::Options {
            listen: listener.local_addr().unwrap(),
            store: root.path().to_owned(),
            advertise: None,
            pins: vec![(NAME.into(), envelope::signer_public(&SIGNER).unwrap())],
            hold: Duration::from_millis(50),
            keep_awake: false,
        };
        let peer = Peer::new();
        // The peer is this test's own, which answers: its pull waits the hang detector.
        let running = server::start_with_peer_pull(listener, &options, HANG).unwrap();
        let fixture = Self {
            running: Some(running),
            peer,
            path,
            _root: root,
        };
        // Own Running before any assertion can panic so Drop stops it first.
        let expected = Store::for_vault(NAME)
            .unwrap()
            .under(&[envelope::signer_public(&SIGNER).unwrap()])
            .recovering(&bytes);
        let actual = fixture.server().stored_records(NAME);
        assert_eq!(actual.inventory(), expected.inventory());
        assert_eq!(actual.complete_inventory(), expected.complete_inventory());
        assert_eq!(actual.encode(), expected.encode());
        fixture
    }

    fn server(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    fn snapshot(&self) -> Snapshot {
        assert_eq!(
            std::fs::symlink_metadata(&self.path).unwrap().mode() & 0o777,
            0o600
        );
        let store = self.server().stored_records(NAME);
        Snapshot {
            bytes: std::fs::read(&self.path).unwrap(),
            inventory: store.inventory(),
            complete: store.complete_inventory(),
            heads: store.log().unwrap().heads(),
            on_file: self.server().test_records_on_file(NAME),
            stamp: fsx::stamp(&self.path).unwrap(),
        }
    }

    fn assert_complete(&self, expected: &[Record]) {
        let snapshot = self.snapshot();
        let ids: BTreeSet<_> = expected.iter().map(|record| *record.id()).collect();
        assert_eq!(snapshot.inventory, ids);
        assert_eq!(snapshot.complete, ids);
        assert_eq!(
            snapshot.bytes,
            wire(expected.iter()),
            "exact canonical signed bytes"
        );
        let store = self.server().stored_records(NAME);
        for record in expected {
            assert_eq!(store.complete_bytes(record.id()), Some(record.bytes()));
        }
    }

    fn pull(&self) -> usize {
        self.server().pull_from_peers(&[self.peer.discovered()])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

fn wire<'a>(records: impl IntoIterator<Item = &'a Record>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend_from_slice(record.bytes());
    }
    bytes
}

fn hex(id: &RecordId) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    for byte in id {
        write!(&mut text, "{byte:02x}").unwrap();
    }
    text
}

struct Observation {
    gross: usize,
    decodes: usize,
    bound: usize,
    wire: usize,
}

fn measured(framing: Framing) -> Observation {
    let history = History::new(VALUES);
    assert_eq!(history.records.len(), 15);
    let fixture = Fixture::new(&history.records);
    let mut frames: Vec<_> = history.records.iter().map(Frame::of).collect();
    frames.push(Frame::of(&history.records[0]));
    frames.push(Frame::of(history.records.last().unwrap()));
    let length: usize = frames.iter().map(|frame| frame.bytes.len()).sum();
    assert_eq!(frames.len(), 17);
    assert!(length > 6 * 1024 * 1024 && length < 8 * 1024 * 1024);
    let expected_ids: Vec<_> = frames.iter().map(|frame| frame.id).collect();
    let mut body_hash = Sha256::new();
    for frame in &frames {
        body_hash.update(&frame.bytes);
    }
    let body_digest = hex(&body_hash.finalize());
    fixture.peer.offer(frames, false, framing, None);
    assert_eq!(fixture.pull(), 0); // Exactly one unmeasured real duplicate pull.
    let before = fixture.snapshot();
    assert_eq!(before.on_file, Some(history.records.len()));
    let decodes_before = test_record_decode_attempts();
    GROSS.store(0, Ordering::SeqCst);
    assert!(!ACTIVE.swap(true, Ordering::SeqCst));
    let measuring = Measuring;
    let result = fixture.pull();
    drop(measuring);
    let gross = GROSS.load(Ordering::SeqCst);
    let decodes = test_record_decode_attempts() - decodes_before;
    assert_eq!(result, 0);
    let traffic = fixture.peer.traffic();
    assert_eq!(
        traffic.len(),
        4,
        "one envelope and one records GET per pull"
    );
    assert_eq!(traffic.iter().filter(|request| request.records).count(), 2);
    assert!(traffic.iter().all(|request| request.finished));
    for request in traffic.iter().filter(|request| request.records) {
        assert_eq!(request.ids, expected_ids);
        assert_eq!(request.bytes, length);
    }
    assert_eq!(
        fixture.snapshot(),
        before,
        "duplicate pull changed retained or durable state"
    );
    fixture.assert_complete(&history.records);
    let held = fixture.server().stored_records(NAME);
    let entries = held
        .log()
        .unwrap()
        .open_entries(&history.identity)
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
    let bound = length + 2 * MAX_RECORD + 1024 * 1024;
    println!(
        "server acquisition {framing:?}: wire={length} sha256={body_digest} frames=17 decode_attempts={decodes} gross_server_bytes={gross} bound={bound}"
    );
    Observation {
        gross,
        decodes,
        bound,
        wire: length,
    }
}

fn waiting_owner_promotes() -> usize {
    let mut history = History::new(0);
    let genesis = history.records[0].clone();
    let parent = history.mint("missing-parent");
    let child = history.mint("waiting-child");
    let successor = history.mint("successor");
    let fixture = Fixture::new(&[genesis, child.clone()]);
    let before = fixture.server().stored_records(NAME);
    assert_eq!(before.len(), 2);
    assert!(before.complete_bytes(child.id()).is_none());
    fixture.peer.offer(
        vec![Frame::of(&child), Frame::of(&parent), Frame::of(&successor)],
        false,
        Framing::Chunked,
        None,
    );
    let before_decodes = test_record_decode_attempts();
    let advanced = fixture.pull();
    let decodes = test_record_decode_attempts() - before_decodes;
    assert_eq!(advanced, 1);
    fixture.assert_complete(&history.records);
    assert_eq!(
        fixture.server().test_records_on_file(NAME),
        Some(history.records.len())
    );
    decodes
}

fn unreadable_memo(bad_signature: bool) {
    let mut history = History::new(0);
    let fixture = Fixture::new(&history.records);
    let good = history.mint("good");
    let hidden = history.mint("hidden");
    let mut bytes = hidden.bytes().to_vec();
    if bad_signature {
        *bytes.last_mut().unwrap() ^= 1;
    } else {
        bytes[37] = 0xee;
    }
    assert!(Record::decode(&bytes).is_err());
    let bad = Frame::raw(bytes, hidden.parents().to_vec());
    let child = aterm_secrets::membership_log::Draft {
        network: *history.records[0].id(),
        sequence: 3,
        previous: Some(bad.id),
        parents: vec![bad.id],
        authored_ms: 200,
        body: Body::Name {
            name: "unreadable-child".into(),
        },
    }
    .sign(&WRITER, INCARNATION)
    .unwrap();
    let leaf = aterm_secrets::membership_log::Draft {
        network: *history.records[0].id(),
        sequence: 4,
        previous: Some(*child.id()),
        parents: vec![*child.id()],
        authored_ms: 201,
        body: Body::Name {
            name: "unreadable-leaf".into(),
        },
    }
    .sign(&WRITER, INCARNATION)
    .unwrap();
    fixture.peer.offer(
        vec![
            Frame::of(&history.records[0]),
            Frame::of(&leaf),
            Frame::of(&good),
            bad.clone(),
            Frame::of(&child),
        ],
        true,
        Framing::Chunked,
        None,
    );
    assert_eq!(fixture.pull(), 1);
    fixture.assert_complete(&history.records[..2]);
    let store = fixture.server().stored_records(NAME);
    for id in [&bad.id, child.id(), leaf.id()] {
        assert!(!store.contains(id));
    }
    assert_eq!(fixture.pull(), 0);
    let records: Vec<_> = fixture
        .peer
        .traffic()
        .into_iter()
        .filter(|request| request.records)
        .collect();
    assert_eq!(records.len(), 2);
    assert!(records[0].ids.contains(&bad.id));
    assert_eq!(records[1].have.first(), Some(&hex(leaf.id())));
    assert!(
        records[1].ids.is_empty(),
        "memo's leaf covers the independently walked branch"
    );
    fixture.assert_complete(&history.records[..2]);
}

fn broken_input_retries(framing: Framing, layout_broken: bool) {
    let mut history = History::new(0);
    let fixture = Fixture::new(&history.records);
    let initial = fixture.snapshot();
    let good = history.mint("after-broken-input");
    let mut frames = vec![Frame::of(&history.records[0]), Frame::of(&good)];
    if layout_broken {
        frames.push(Frame::raw(b"ALM".to_vec(), Vec::new()));
    }
    fixture.peer.offer(frames, true, framing, None);
    assert_eq!(fixture.pull(), 0);
    assert_eq!(fixture.snapshot(), initial);
    fixture.peer.offer(
        history.records.iter().map(Frame::of).collect(),
        true,
        Framing::Length,
        None,
    );
    assert_eq!(fixture.pull(), 1);
    let request = fixture
        .peer
        .traffic()
        .into_iter()
        .find(|request| request.records)
        .unwrap();
    assert!(
        !request.have.contains(&hex(good.id())),
        "failed acquisition cannot memoize a valid prefix"
    );
    assert!(request.ids.contains(good.id()));
    fixture.assert_complete(&history.records);
}

fn current_file_is_refreshed() {
    let mut history = History::new(0);
    history.mint("already-held");
    let fixture = Fixture::new(&history.records);
    let known = history.records[1].clone();
    let incoming = history.mint("after-file-change");
    let path = fixture.path.clone();
    let earlier = history.records[0].bytes().to_vec();
    fixture.peer.offer(
        vec![Frame::of(&known), Frame::of(&incoming)],
        false,
        Framing::Length,
        Some(Box::new(move || {
            fsx::write_atomic(&path, &earlier, 0o600).unwrap()
        })),
    );
    assert_eq!(fixture.pull(), 1);
    // Complete in-memory history is monotone. The novel successor forces a full
    // write after the shortened-file refresh clears on_file; this is not eviction.
    fixture.assert_complete(&history.records);
    assert_eq!(
        fixture.server().test_records_on_file(NAME),
        Some(history.records.len())
    );
}

fn failed_write_is_not_judged() {
    let mut history = History::new(0);
    let fixture = Fixture::new(&history.records);
    let original = std::fs::read(&fixture.path).unwrap();
    let incoming = history.mint("take-after-write-repair");
    std::fs::remove_file(&fixture.path).unwrap();
    std::fs::create_dir(&fixture.path).unwrap();
    std::fs::write(fixture.path.join("preserved"), b"obstruction").unwrap();
    fixture.peer.offer(
        history.records.iter().map(Frame::of).collect(),
        true,
        Framing::Length,
        None,
    );
    assert_eq!(fixture.pull(), 0);
    assert_eq!(
        std::fs::read(fixture.path.join("preserved")).unwrap(),
        b"obstruction"
    );
    assert!(
        !fixture
            .server()
            .stored_records(NAME)
            .contains(incoming.id())
    );
    std::fs::remove_file(fixture.path.join("preserved")).unwrap();
    std::fs::remove_dir(&fixture.path).unwrap();
    fsx::write_atomic(&fixture.path, &original, 0o600).unwrap();
    assert_eq!(fixture.pull(), 1);
    let requests: Vec<_> = fixture
        .peer
        .traffic()
        .into_iter()
        .filter(|request| request.records)
        .collect();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(!request.have.contains(&hex(incoming.id())));
        assert!(request.ids.contains(incoming.id()));
    }
    fixture.assert_complete(&history.records);
}

#[test]
fn periodic_peer_pull_reuses_known_records_and_bounds_acquisition() {
    let declared = measured(Framing::Length);
    let chunked = measured(Framing::Chunked);
    let waiting_decodes = waiting_owner_promotes();
    unreadable_memo(false);
    unreadable_memo(true);
    broken_input_retries(Framing::Length, true);
    broken_input_retries(Framing::ShortLength, true);
    broken_input_retries(Framing::BadChunk, true);
    broken_input_retries(Framing::ShortLength, false);
    broken_input_retries(Framing::BadChunk, false);
    current_file_is_refreshed();
    failed_write_is_not_judged();
    println!(
        "all periodic peer-pull state, commit, unreadable-descendant, and memo controls passed; waiting_novel_decode_attempts={waiting_decodes}"
    );
    assert_eq!(declared.wire, chunked.wire);
    assert_eq!(
        declared.decodes, 0,
        "known declared-response records were reverified"
    );
    assert_eq!(
        chunked.decodes, 0,
        "known chunked-response records were reverified"
    );
    assert_eq!(
        waiting_decodes, 2,
        "only the missing parent and successor need verification"
    );
    assert!(
        declared.gross <= declared.bound,
        "declared acquisition gross {} > {}",
        declared.gross,
        declared.bound
    );
    assert!(
        chunked.gross <= chunked.bound,
        "chunked acquisition gross {} > {}",
        chunked.gross,
        chunked.bound
    );
}
