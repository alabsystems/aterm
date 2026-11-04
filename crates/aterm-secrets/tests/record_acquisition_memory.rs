// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Gross client allocation and completion semantics of real streamed records pulls.

#![cfg(unix)]

use aterm_digest::Sha256;
use aterm_secrets::{
    SecretBytes,
    age::Identity,
    b64, client,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Body, Draft, Entrant, EpochAudience, Inserted, Log, MAX_RECORD, MAX_RECORDS_STREAM, Record,
        RecordId, decode_records, test_record_decode_attempts,
    },
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static GROSS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    // Only the bounded fixture server opts out. Every client worker is counted.
    static FIXTURE_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn allocated(bytes: usize) {
    if ACTIVE.load(Ordering::SeqCst) && !FIXTURE_THREAD.try_with(Cell::get).unwrap_or(false) {
        GROSS.fetch_add(bytes, Ordering::SeqCst);
    }
}

struct Counting;

// SAFETY: forward original pointers and layouts to System. Atomics and const TLS
// allocate nothing. Count successful requests, including realloc's new size;
// never subtract frees, including frees of pre-window cache and memo owners.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forward the matching live allocation and requested size.
        let changed = unsafe { System.realloc(pointer, layout, new_size) };
        if !changed.is_null() {
            allocated(new_size);
        }
        changed
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forward the allocation and its matching original layout.
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

const NAME: &str = "record-acquisition";
const WRITER: [u8; 32] = [51; 32];
const INCARNATION: [u8; 16] = [52; 16];
const SIGNER: [u8; 32] = [53; 32];
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const HANG: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
enum Framing {
    Length,
    Chunked,
    ShortLength,
    BadChunk,
    ShortTrailer,
    OversizedLength,
    OversizedChunk,
}

type BeforeBody = Box<dyn FnOnce() + Send>;

struct Answer {
    bytes: Arc<Vec<u8>>,
    status: u16,
    framing: Framing,
    before: Option<(usize, BeforeBody)>,
    allow_peek_close: bool,
}

#[derive(Clone)]
struct Get {
    line: String,
    finished: bool,
    peek_closed: bool,
}

struct HostState {
    answer: Answer,
    gets: Vec<Get>,
    errors: Vec<String>,
}

struct Host {
    addr: SocketAddr,
    state: Arc<Mutex<HostState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    done: Receiver<()>,
    completed: Cell<usize>,
}

impl Host {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(HostState {
            answer: Answer {
                bytes: Arc::new(Vec::new()),
                status: 204,
                framing: Framing::Length,
                before: None,
                allow_peek_close: false,
            },
            gets: Vec::new(),
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

    fn url(&self) -> String {
        format!("http://{}/v1/{NAME}", self.addr)
    }

    fn answer(&self, bytes: Vec<u8>, status: u16, framing: Framing) {
        self.traffic();
        let mut state = self.state.lock().unwrap();
        state.answer = Answer {
            bytes: Arc::new(bytes),
            status,
            framing,
            before: None,
            allow_peek_close: false,
        };
        state.gets.clear();
        state.errors.clear();
        self.completed.set(0);
    }

    fn traffic(&self) -> Vec<Get> {
        let requested = self.state.lock().unwrap().gets.len();
        while self.completed.get() < requested {
            self.done
                .recv_timeout(HANG)
                .expect("fixture request must finish");
            self.completed.set(self.completed.get() + 1);
        }
        let state = self.state.lock().unwrap();
        assert!(
            state.errors.is_empty(),
            "fixture errors: {:?}",
            state.errors
        );
        state.gets.clone()
    }

    fn have(&self) -> Vec<String> {
        let gets = self.traffic();
        let target = gets.last().unwrap().line.split_whitespace().nth(1).unwrap();
        target
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|part| part.strip_prefix("have="))
            .unwrap()
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn serve(stream: &mut TcpStream, state: &Mutex<HostState>) -> io::Result<()> {
    stream.set_read_timeout(Some(HANG))?;
    stream.set_write_timeout(Some(HANG))?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16 * 1024 {
            return Err(invalid("request header too long"));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let text = std::str::from_utf8(&header).map_err(io::Error::other)?;
    let line = text.split("\r\n").next().unwrap();
    if !line.starts_with(&format!("GET /v1/{NAME}/records?")) {
        return Err(invalid("unexpected request"));
    }
    let (index, bytes, status, framing, before, allow_close) = {
        let mut state = state.lock().unwrap();
        let index = state.gets.len();
        state.gets.push(Get {
            line: line.to_owned(),
            finished: false,
            peek_closed: false,
        });
        let before = if state
            .answer
            .before
            .as_ref()
            .is_some_and(|(nth, _)| *nth == index + 1)
        {
            state.answer.before.take().map(|(_, action)| action)
        } else {
            None
        };
        (
            index,
            Arc::clone(&state.answer.bytes),
            state.answer.status,
            state.answer.framing,
            before,
            (state.answer.allow_peek_close && index == 0)
                || matches!(state.answer.framing, Framing::OversizedLength),
        )
    };
    if let Some(before) = before {
        before();
    }
    let sent = send_answer(stream, &bytes, status, framing);
    match sent {
        Ok(()) => state.lock().unwrap().gets[index].finished = true,
        Err(error)
            if allow_close
                && matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                ) =>
        {
            state.lock().unwrap().gets[index].peek_closed = true;
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

fn send_answer(
    stream: &mut TcpStream,
    bytes: &[u8],
    status: u16,
    framing: Framing,
) -> io::Result<()> {
    let phrase = match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        _ => unreachable!(),
    };
    write!(
        stream,
        "HTTP/1.1 {status} {phrase}\r\nContent-Type: application/octet-stream\r\nX-Aterm-Records-Need: 0\r\nX-Aterm-Records-Boot: 00000000000000000000000000000001\r\nConnection: close\r\n"
    )?;
    match framing {
        Framing::Length | Framing::ShortLength | Framing::OversizedLength => {
            let declared = match framing {
                Framing::ShortLength => bytes.len() + 19,
                Framing::OversizedLength => MAX_RECORDS_STREAM + 1,
                _ => bytes.len(),
            };
            write!(stream, "Content-Length: {declared}\r\n\r\n")?;
            for chunk in bytes.chunks(32 * 1024) {
                stream.write_all(chunk)?;
            }
        }
        Framing::Chunked | Framing::BadChunk | Framing::ShortTrailer | Framing::OversizedChunk => {
            stream.write_all(b"Transfer-Encoding: chunked\r\n\r\n")?;
            // An odd chunk size deliberately splits record headers and ciphertext.
            for chunk in bytes.chunks(32 * 1024 + 7) {
                write!(stream, "{:x}\r\n", chunk.len())?;
                stream.write_all(chunk)?;
                stream.write_all(b"\r\n")?;
            }
            if matches!(framing, Framing::OversizedChunk) {
                // The announced next chunk exceeds the real response limit.
                // No huge fixture allocation or transmission is needed.
                write!(stream, "{:x}\r\n", MAX_RECORDS_STREAM + 1)?;
            } else {
                stream.write_all(match framing {
                    Framing::BadChunk => b"not-hex\r\n",
                    Framing::ShortTrailer => b"0\r\nX-Complete: yes\r\n",
                    _ => b"0\r\nX-Complete: yes\r\n\r\n",
                })?;
            }
        }
    }
    Ok(())
}

struct History {
    records: Vec<Record>,
    wire: Vec<u8>,
    local: Identity,
}

fn history(paths: &client::Paths, values: usize) -> History {
    let local = client::machine_identity(paths).unwrap();
    let recipient = Identity::from_secret_bytes([54; 32]);
    let request = |identity: &Identity, writer: &[u8; 32], machine, incarnation| {
        Request::create([55; 16], [56; 32], machine, incarnation, identity, writer).unwrap()
    };
    let writer_request = request(&recipient, &WRITER, [57; 16], INCARNATION);
    let actor = writer_request.binding().actor();
    let local_request = request(&local, &[58; 32], [59; 16], [60; 16]);
    let local_actor = local_request.binding().actor();
    let genesis = Record::found(
        &SIGNER,
        vec![
            Entrant {
                request: writer_request,
                node_key: [61; 32],
                name: "writer".into(),
                label: "m1".into(),
            },
            Entrant {
                request: local_request,
                node_key: [62; 32],
                name: "local".into(),
                label: "m2".into(),
            },
        ],
        1,
    )
    .unwrap();
    let mut log = Log::new(genesis.clone()).unwrap();
    let mut records = vec![genesis];
    let (draft, key) = log
        .draft_epoch(&actor, 2, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&WRITER, INCARNATION).unwrap();
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch.clone()).unwrap(), Inserted::Held);
    records.push(epoch);
    let wrap = log
        .draft_wrap(&actor, 3, epoch_id, &key, recipient.recipient())
        .unwrap()
        .sign(&WRITER, INCARNATION)
        .unwrap();
    assert_eq!(log.insert(wrap.clone()).unwrap(), Inserted::Held);
    records.push(wrap);
    for i in 0..values {
        let entry = Entry {
            name: format!("value-{i}"),
            value: SecretBytes::new(vec![i as u8; VALUE_BYTES]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let record = log
            .draft_put(&actor, 4 + i as u64, epoch_id, &key, &entry)
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
        records.push(record);
    }
    assert_eq!(log.held_count(), values + 3);
    assert!(log.membership().members().contains_key(&local_actor));
    assert!(log.unwrap_epochs(&local).unwrap().is_empty());
    assert!(log.open_entries(&local).unwrap().is_none());
    let entries = log.open_entries(&recipient).unwrap().unwrap();
    assert_eq!(entries.len(), values);
    for i in 0..values {
        let entry = entries
            .iter()
            .find(|entry| entry.name == format!("value-{i}"))
            .unwrap();
        assert_eq!(entry.value.len(), VALUE_BYTES);
        assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
        assert_ne!(entry.name, aterm_secrets::SWARM_POLICY_ENTRY);
    }
    let wire = wire(records.iter());
    assert_eq!(decode_records(&wire).unwrap().len(), records.len());
    History {
        records,
        wire,
        local,
    }
}

struct Fixture {
    host: Host,
    paths: client::Paths,
    history: History,
    _root: aterm_tempfile::TempDir,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    wire: Vec<u8>,
    inventory: BTreeSet<RecordId>,
    complete: BTreeSet<RecordId>,
    heads: Vec<RecordId>,
    stamp: fsx::FileStamp,
}

impl Fixture {
    fn new(whole: bool, values: usize) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = client::Paths::for_home(&root.path().join("member"));
        let history = history(&paths, values);
        let host = Host::start();
        client::configure_log_member(
            &paths,
            &format!(
                "{}#{}",
                host.url(),
                b64::encode_padded(&envelope::signer_public(&SIGNER).unwrap())
            ),
            "local",
        )
        .unwrap();
        let config = client::config(&paths).unwrap().unwrap();
        assert_eq!(config.name, NAME);
        assert_eq!(
            config.signer,
            Some(envelope::signer_public(&SIGNER).unwrap())
        );
        assert!(!client::alab_vault(&config));
        let initial = if whole {
            &history.wire
        } else {
            history.records[0].bytes()
        };
        assert_eq!(
            client::save_records(&paths, initial).unwrap(),
            if whole { values + 3 } else { 1 }
        );
        let fixture = Self {
            host,
            paths,
            history,
            _root: root,
        };
        fixture.warm();
        fixture
    }

    fn warm(&self) {
        let first = client::load_records(&self.paths).unwrap();
        let second = client::load_records(&self.paths).unwrap();
        assert_eq!(first.inventory(), second.inventory());
        assert_eq!(
            client::read_machine_identity(&self.paths)
                .unwrap()
                .unwrap()
                .recipient(),
            self.history.local.recipient()
        );
    }

    fn snapshot(&self) -> Snapshot {
        let path = self.paths.records_file();
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().mode() & 0o777,
            0o600
        );
        let store = client::load_records(&self.paths).unwrap();
        Snapshot {
            wire: std::fs::read(&path).unwrap(),
            inventory: store.inventory(),
            complete: store.complete_inventory(),
            heads: store.log().unwrap().heads(),
            stamp: fsx::stamp(&path).unwrap(),
        }
    }

    fn assert_records(&self, records: &[&Record]) {
        let expected = wire(records.iter().copied());
        let current = self.snapshot();
        assert_eq!(current.wire, expected, "exact canonical signed bytes");
        let ids: BTreeSet<_> = records.iter().map(|record| *record.id()).collect();
        assert_eq!(current.inventory, ids);
        assert_eq!(current.complete, ids);
        let store = client::load_records(&self.paths).unwrap();
        for record in records {
            assert_eq!(store.get_bytes(record.id()), Some(record.bytes()));
        }
    }

    fn sync(&self) -> aterm_secrets::Result<usize> {
        let before = self.host.traffic().len();
        let result = client::test_sync_records_without_discovery(&self.paths);
        assert_eq!(self.host.traffic().len(), before + 1);
        result
    }

    fn name(
        &self,
        name: &str,
        sequence: u64,
        previous: Option<RecordId>,
        parents: Vec<RecordId>,
    ) -> Record {
        Draft {
            network: *self.history.records[0].id(),
            sequence,
            previous,
            parents,
            authored_ms: 100 + sequence,
            body: Body::Name { name: name.into() },
        }
        .sign(&WRITER, INCARNATION)
        .unwrap()
    }

    fn root_name(&self, name: &str) -> Record {
        self.name(name, 1, None, vec![*self.history.records[0].id()])
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

fn measured(framing: Framing) -> (usize, usize, usize) {
    let fixture = Fixture::new(true, VALUES);
    let mut body = fixture.history.wire.clone();
    body.extend_from_slice(fixture.history.records.last().unwrap().bytes());
    body.extend_from_slice(fixture.history.records[0].bytes());
    let frames = fixture.history.records.len() + 2;
    let length = body.len();
    assert!(frames <= 32 && length > 6 * 1024 * 1024 && length < 8 * 1024 * 1024);
    assert_eq!(decode_records(&body).unwrap().len(), frames);
    let digest = Sha256::digest(&body);
    fixture.host.answer(body, 200, framing);
    assert_eq!(fixture.sync().unwrap(), 0); // Declared unmeasured actual-sync warmup.
    fixture.warm();
    let before = fixture.snapshot();
    let count_before = test_record_decode_attempts();
    GROSS.store(0, Ordering::SeqCst);
    assert!(!ACTIVE.swap(true, Ordering::SeqCst));
    let measuring = Measuring;
    let result = client::test_sync_records_without_discovery(&fixture.paths);
    drop(measuring);
    let gross = GROSS.load(Ordering::SeqCst);
    let attempts = test_record_decode_attempts() - count_before;
    assert_eq!(result.unwrap(), 0);
    assert_eq!(attempts, frames, "known frames still require verification");
    let gets = fixture.host.traffic();
    assert_eq!(gets.len(), 2);
    assert!(gets.iter().all(|get| get.finished && !get.peek_closed));
    let have = client::load_records(&fixture.paths).unwrap().have();
    assert_eq!(
        fixture.host.have(),
        have.iter().map(hex).collect::<Vec<_>>()
    );
    assert_eq!(
        fixture.snapshot(),
        before,
        "duplicate response changed local state"
    );
    assert_eq!(
        Sha256::digest(fixture.host.state.lock().unwrap().answer.bytes.as_slice()),
        digest
    );
    let bound = 3 * length + 2 * MAX_RECORD + 1024 * 1024;
    println!(
        "acquisition {framing:?}: wire={length} frames={frames} decode_attempts={attempts} gross_client_bytes={gross} bound={bound}"
    );
    (gross, bound, length)
}

fn unreadable_large_frame(bad_signature: bool) {
    let fixture = Fixture::new(false, 1);
    let mut hidden = fixture.history.records.last().unwrap().bytes().to_vec();
    // Use a complete large Put frame. Descendants refer to its exact received
    // digest, independent of its invalidity and unavailable original parents.
    if bad_signature {
        *hidden.last_mut().unwrap() ^= 1;
    } else {
        assert_eq!(&hidden[..5], b"ALM1\x01");
        hidden[37] = 0xfe;
    }
    assert!(hidden.len() > 512 * 1024);
    assert!(Record::decode(&hidden).is_err());
    let id = Sha256::digest(&hidden);
    let child = fixture.name(
        "child",
        2,
        Some(id),
        vec![id, *fixture.history.records[0].id()],
    );
    let leaf = fixture.name(
        "leaf",
        3,
        Some(*child.id()),
        vec![*child.id(), *fixture.history.records[0].id()],
    );
    let good = fixture.root_name("good");
    let mut bytes = wire([&leaf, &good]);
    bytes.extend_from_slice(&hidden);
    bytes.extend_from_slice(child.bytes());
    fixture.host.answer(bytes, 200, Framing::Chunked);
    assert_eq!(fixture.sync().unwrap(), 1);
    fixture.assert_records(&[&fixture.history.records[0], &good]);
    let store = client::load_records(&fixture.paths).unwrap();
    for id in [&id, child.id(), leaf.id()] {
        assert!(!store.contains(id));
    }
    fixture.host.answer(Vec::new(), 204, Framing::Length);
    assert_eq!(fixture.sync().unwrap(), 0);
    assert_eq!(fixture.host.have().first(), Some(&hex(leaf.id())));
}

fn transport_failure_after_layout(framing: Framing, status: u16) {
    let fixture = Fixture::new(false, 1);
    let good = fixture.root_name("after-failure");
    let mut bytes = good.bytes().to_vec();
    bytes.extend_from_slice(b"invalid membership layout");
    fixture.host.answer(bytes.clone(), status, framing);
    // The retained collected API is an independent transport-category oracle.
    let http = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        HANG,
    );
    let failed = http
        .get(&format!("{}/records?have=", fixture.host.url()))
        .limit(MAX_RECORDS_STREAM)
        .send_phased(&[])
        .unwrap_err();
    assert_eq!(failed.phase, aterm_http::Phase::Answered);
    match framing {
        Framing::ShortLength | Framing::ShortTrailer => assert!(
            matches!(&failed.error, aterm_http::Error::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof)
        ),
        Framing::BadChunk => {
            assert!(matches!(&failed.error, aterm_http::Error::Protocol(_)))
        }
        Framing::OversizedLength | Framing::OversizedChunk => assert!(
            matches!(&failed.error, aterm_http::Error::TooLarge { limit } if *limit == MAX_RECORDS_STREAM)
        ),
        _ => unreachable!(),
    }
    let reason = failed.error.to_string();
    let before = fixture.snapshot();
    fixture.host.answer(bytes, status, framing);
    let error = fixture.sync().unwrap_err();
    assert_eq!(error.kind(), aterm_secrets::ErrorKind::Other);
    assert!(
        error.to_string().contains(&reason),
        "transport precedence: {error:?}, expected {reason}"
    );
    assert_eq!(fixture.snapshot(), before);
    fixture
        .host
        .answer(good.bytes().to_vec(), 200, Framing::Chunked);
    assert_eq!(fixture.sync().unwrap(), 1);
    assert_eq!(
        fixture.host.have(),
        vec![hex(fixture.history.records[0].id())]
    );
    fixture.assert_records(&[&fixture.history.records[0], &good]);
}

fn clean_status_and_layout() {
    let fixture = Fixture::new(false, 1);
    let before = fixture.snapshot();
    for status in [204, 404] {
        fixture.host.answer(Vec::new(), status, Framing::Length);
        assert_eq!(fixture.sync().unwrap(), 0);
        assert_eq!(fixture.snapshot(), before);
    }
    fixture
        .host
        .answer(b"denied".to_vec(), 403, Framing::Chunked);
    let error = fixture.sync().unwrap_err();
    assert!(error.to_string().contains("answered 403"));
    assert_eq!(fixture.snapshot(), before);
    let good = fixture.root_name("after-layout");
    let mut bytes = good.bytes().to_vec();
    bytes.extend_from_slice(b"ALM");
    fixture.host.answer(bytes, 200, Framing::Chunked);
    assert!(fixture.sync().is_err());
    assert_eq!(fixture.snapshot(), before);
    fixture
        .host
        .answer(good.bytes().to_vec(), 200, Framing::Length);
    assert_eq!(fixture.sync().unwrap(), 1);
    assert_eq!(
        fixture.host.have(),
        vec![hex(fixture.history.records[0].id())]
    );
    fixture.assert_records(&[&fixture.history.records[0], &good]);
}

fn peer_retry(complete: bool) {
    let fixture = Fixture::new(false, 1);
    let peer = Host::start();
    let bytes = fixture.history.wire.clone();
    assert!(bytes.len() > 256 * 1024);
    peer.answer(
        bytes,
        200,
        if complete {
            Framing::Chunked
        } else {
            Framing::ShortTrailer
        },
    );
    let observed = Arc::new(AtomicBool::new(false));
    let observation = Arc::clone(&observed);
    let path = fixture.paths.records_file();
    let initial = fixture.history.records[0].bytes().to_vec();
    {
        let mut state = peer.state.lock().unwrap();
        state.answer.allow_peek_close = true;
        state.answer.before = Some((
            2,
            Box::new(move || {
                assert_eq!(
                    std::fs::read(path).unwrap(),
                    initial,
                    "peek published a prefix"
                );
                observation.store(true, Ordering::SeqCst);
            }),
        ));
    }
    let result = client::sync_records_with_candidates(&fixture.paths, None, &[peer.url()]);
    assert!(observed.load(Ordering::SeqCst), "whole retry did not occur");
    assert_eq!(fixture.host.traffic().len(), 1);
    let traffic = peer.traffic();
    assert_eq!(traffic.len(), 2, "exactly one peek and one whole retry");
    assert!(traffic[0].finished || traffic[0].peek_closed);
    assert!(traffic[1].finished);
    if complete {
        assert_eq!(result.unwrap(), fixture.history.records.len() - 1);
        fixture.assert_records(&fixture.history.records.iter().collect::<Vec<_>>());
    } else {
        // The primary's clean 204 still makes this an answered pass.
        assert_eq!(result.unwrap(), 0);
        fixture.assert_records(&[&fixture.history.records[0]]);
        peer.answer(fixture.history.wire.clone(), 200, Framing::Chunked);
        peer.state.lock().unwrap().answer.allow_peek_close = true;
        assert_eq!(
            client::sync_records_with_candidates(&fixture.paths, None, &[peer.url()]).unwrap(),
            fixture.history.records.len() - 1
        );
        fixture.assert_records(&fixture.history.records.iter().collect::<Vec<_>>());
    }
}

#[test]
fn real_sync_bounds_acquisition_and_preserves_completion() {
    let declared = measured(Framing::Length);
    let chunked = measured(Framing::Chunked);
    unreadable_large_frame(false);
    unreadable_large_frame(true);
    clean_status_and_layout();
    for status in [200, 403, 404] {
        for framing in [
            Framing::ShortLength,
            Framing::BadChunk,
            Framing::ShortTrailer,
            Framing::OversizedLength,
            Framing::OversizedChunk,
        ] {
            transport_failure_after_layout(framing, status);
        }
    }
    peer_retry(true);
    peer_retry(false);
    println!(
        "all acquisition transport, status, unreadable-descendant, and peer retry controls passed"
    );
    assert_eq!(declared.2, chunked.2);
    assert!(
        declared.0 <= declared.1,
        "declared body gross allocations {} > {}",
        declared.0,
        declared.1
    );
    assert!(
        chunked.0 <= chunked.1,
        "chunked body gross allocations {} > {}",
        chunked.0,
        chunked.1
    );
}
