// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Gross allocations of real ciphertext replication: no due post, Whole, and large Past.

#![cfg(unix)]

use aterm_digest::Sha256;
use aterm_secrets::{
    SecretBytes,
    age::Identity,
    b64, client,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Body, Entrant, EpochAudience, Inserted, Log, MemberId, Record, RecordId, Store,
        decode_records,
    },
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static GROSS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // Only the fixture server sets this. Client workers retain the default false.
    static FIXTURE_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn allocated(bytes: usize) {
    if ACTIVE.load(Ordering::SeqCst) && !FIXTURE_THREAD.try_with(Cell::get).unwrap_or(false) {
        GROSS.fetch_add(bytes, Ordering::SeqCst);
    }
}

struct Counting;

// SAFETY: every allocator call forwards the original pointer/layout to System.
// Atomics and const TLS allocate nothing. Count successful requested sizes,
// including the new size of realloc; never subtract frees or old ownership.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forward the live allocation and requested new size unchanged.
        let changed = unsafe { System.realloc(pointer, layout, new_size) };
        if !changed.is_null() {
            allocated(new_size);
        }
        changed
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forward the matching allocation/layout unchanged.
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

const NAME: &str = "outbound-sync-memory";
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const BOUND: usize = 512 * 1024;
const HANG: Duration = Duration::from_secs(60);
const WRITER: [u8; 32] = [17; 32];
const INCARNATION: [u8; 16] = [18; 16];
const SIGNER: [u8; 32] = [23; 32];

#[derive(Clone, Debug)]
struct Posted {
    length: usize,
    digest: [u8; 32],
    exact: bool,
}

struct HostState {
    boot: u64,
    expected: Arc<Vec<u8>>,
    gets: Vec<String>,
    posts: Vec<Posted>,
    errors: Vec<String>,
}

#[derive(Clone)]
struct Traffic {
    gets: Vec<String>,
    posts: Vec<Posted>,
    errors: Vec<String>,
}

struct Host {
    addr: SocketAddr,
    state: Arc<Mutex<HostState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Host {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(HostState {
            boot: 1,
            expected: Arc::new(Vec::new()),
            gets: Vec::new(),
            posts: Vec::new(),
            errors: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
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
            }
        });
        Self {
            addr,
            state,
            stop,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/v1/{NAME}", self.addr)
    }

    fn expected(&self, wire: &[u8]) {
        self.state.lock().unwrap().expected = Arc::new(wire.to_vec());
    }

    fn traffic(&self) -> Traffic {
        let state = self.state.lock().unwrap();
        Traffic {
            gets: state.gets.clone(),
            posts: state.posts.clone(),
            errors: state.errors.clone(),
        }
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

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn serve(stream: &mut TcpStream, state: &Mutex<HostState>) -> io::Result<()> {
    stream.set_read_timeout(Some(HANG))?;
    stream.set_write_timeout(Some(HANG))?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() == 16 * 1024 {
            return Err(invalid("request header exceeded its bound"));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = std::str::from_utf8(&head).map_err(|_| invalid("non-UTF8 request header"))?;
    let mut lines = head.split("\r\n");
    let request = lines
        .next()
        .ok_or_else(|| invalid("missing request line"))?;
    let headers: BTreeMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    if request.starts_with(&format!("GET /v1/{NAME}/records?")) {
        let boot = {
            let mut state = state.lock().unwrap();
            state.gets.push(request.to_owned());
            state.boot
        };
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\nX-Aterm-Records-Need: 1\r\nX-Aterm-Records-Boot: {boot:032x}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    if request != format!("POST /v1/{NAME}/records HTTP/1.1") {
        return Err(invalid("unexpected request"));
    }
    if headers.get("content-type").map(String::as_str) != Some("application/octet-stream")
        || headers.contains_key("transfer-encoding")
    {
        return Err(invalid("unexpected upload framing or type"));
    }
    let length: usize = headers
        .get("content-length")
        .ok_or_else(|| invalid("missing upload length"))?
        .parse()
        .map_err(|_| invalid("invalid upload length"))?;
    if length > aterm_secrets::membership_log::MAX_RECORDS_STREAM {
        return Err(invalid("upload exceeded the record stream bound"));
    }
    let expected = Arc::clone(&state.lock().unwrap().expected);
    let mut exact = length == expected.len();
    let mut hash = Sha256::new();
    let mut at = 0;
    let mut buffer = [0; 64 * 1024];
    while at < length {
        let take = buffer.len().min(length - at);
        stream.read_exact(&mut buffer[..take])?;
        hash.update(&buffer[..take]);
        if exact {
            exact = buffer[..take] == expected[at..at + take];
        }
        at += take;
    }
    state.lock().unwrap().posts.push(Posted {
        length,
        digest: hash.finalize(),
        exact,
    });
    // Drain the declared upload before replying. Never block awaiting an extra
    // byte while the client is waiting for this response.
    stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

struct History {
    full: Vec<u8>,
    prefix: Vec<u8>,
    tail: Vec<u8>,
    waiting: Record,
    complete: Store,
    local: Identity,
    local_actor: MemberId,
}

fn history(paths: &client::Paths) -> History {
    let local = client::machine_identity(paths).unwrap();
    let recipient = Identity::from_secret_bytes([19; 32]);
    let request = |identity: &Identity, writer: &[u8; 32], machine, incarnation| {
        Request::create([20; 16], [21; 32], machine, incarnation, identity, writer).unwrap()
    };
    let writer_request = request(&recipient, &WRITER, [22; 16], INCARNATION);
    let writer = writer_request.binding().actor();
    let local_request = request(&local, &[27; 32], [28; 16], [29; 16]);
    let local_actor = local_request.binding().actor();
    let genesis = Record::found(
        &SIGNER,
        vec![
            Entrant {
                request: writer_request,
                node_key: [24; 32],
                name: "writer".into(),
                label: "m1".into(),
            },
            Entrant {
                request: local_request,
                node_key: [30; 32],
                name: "local".into(),
                label: "m2".into(),
            },
        ],
        1,
    )
    .unwrap();
    let mut full = genesis.bytes().to_vec();
    let mut log = Log::new(genesis).unwrap();
    let (draft, key) = log
        .draft_epoch(&writer, 2, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&WRITER, INCARNATION).unwrap();
    let epoch_id = *epoch.id();
    full.extend_from_slice(epoch.bytes());
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    let wrap = log
        .draft_wrap(&writer, 3, epoch_id, &key, recipient.recipient())
        .unwrap()
        .sign(&WRITER, INCARNATION)
        .unwrap();
    full.extend_from_slice(wrap.bytes());
    assert_eq!(log.insert(wrap).unwrap(), Inserted::Held);
    let prefix = full.clone();
    let mut tail = Vec::new();
    for i in 0..VALUES {
        let entry = Entry {
            name: format!("large-{i}"),
            value: SecretBytes::new(vec![i as u8; VALUE_BYTES]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let record = log
            .draft_put(&writer, 4 + i as u64, epoch_id, &key, &entry)
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        tail.extend_from_slice(record.bytes());
        full.extend_from_slice(record.bytes());
        assert_eq!(log.insert(record).unwrap(), Inserted::Held);
    }
    assert_eq!(log.held_count(), VALUES + 3);
    assert!(log.membership().members().contains_key(&local_actor));
    assert!(log.unwrap_epochs(&local).unwrap().is_empty());
    assert!(log.open_entries(&local).unwrap().is_none());
    let entries = log.open_entries(&recipient).unwrap().unwrap();
    assert_eq!(entries.len(), VALUES);
    for i in 0..VALUES {
        let entry = entries
            .iter()
            .find(|entry| entry.name == format!("large-{i}"))
            .unwrap();
        assert_eq!(entry.value.len(), VALUE_BYTES);
        assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
        assert_ne!(entry.name, aterm_secrets::SWARM_POLICY_ENTRY);
    }
    assert!(full.len() > VALUES * VALUE_BYTES);
    assert_eq!(decode_records(&full).unwrap().len(), VALUES + 3);
    assert_eq!(decode_records(&tail).unwrap().len(), VALUES);
    let complete = Store::for_vault(NAME).unwrap().recovering(&full);
    assert_eq!(complete.complete_inventory().len(), VALUES + 3);
    assert_eq!(complete.encode_held(), full);
    let parent = log
        .draft(
            &writer,
            100,
            Body::Name {
                name: "missing-parent".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    assert_eq!(log.insert(parent).unwrap(), Inserted::Held);
    let waiting = log
        .draft(
            &writer,
            101,
            Body::Name {
                name: "waiting-child".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    History {
        full,
        prefix,
        tail,
        waiting,
        complete,
        local,
        local_actor,
    }
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    bytes: Vec<u8>,
    inventory: Vec<RecordId>,
    complete: Vec<RecordId>,
    heads: Vec<RecordId>,
    stamp: fsx::FileStamp,
}

struct Fixture {
    host: Host,
    paths: client::Paths,
    history: History,
    _root: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new(prefix_only: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = client::Paths::for_home(&root.path().join("member"));
        let history = history(&paths);
        println!(
            "ciphertext fixture prefix_only={prefix_only} wire={} prefix={} tail={} complete={} waiting=1",
            history.full.len(),
            history.prefix.len(),
            history.tail.len(),
            VALUES + 3
        );
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
        let initial = if prefix_only {
            &history.prefix
        } else {
            &history.full
        };
        let mut wire = initial.to_vec();
        wire.extend_from_slice(history.waiting.bytes());
        fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
        host.expected(initial);
        let fixture = Self {
            host,
            paths,
            history,
            _root: root,
        };
        let loaded = client::load_records(&fixture.paths).unwrap();
        let count = if prefix_only { 3 } else { VALUES + 3 };
        assert_eq!(loaded.complete_inventory().len(), count);
        assert_eq!(loaded.len(), count + 1);
        assert!(
            loaded
                .log()
                .unwrap()
                .membership()
                .members()
                .contains_key(&fixture.history.local_actor)
        );
        assert_eq!(
            client::read_machine_identity(&fixture.paths)
                .unwrap()
                .unwrap()
                .recipient(),
            fixture.history.local.recipient()
        );
        assert!(
            loaded
                .complete_bytes(fixture.history.waiting.id())
                .is_none()
        );
        assert!(
            loaded
                .log()
                .unwrap()
                .unwrap_epochs(&fixture.history.local)
                .unwrap()
                .is_empty()
        );
        assert!(client::load_records(&fixture.paths).unwrap().encode() == wire);
        fixture
    }

    fn snapshot(&self) -> Snapshot {
        let path = self.paths.records_file();
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().mode() & 0o777,
            0o600
        );
        let store = client::load_records(&self.paths).unwrap();
        Snapshot {
            bytes: std::fs::read(&path).unwrap(),
            inventory: store.inventory().into_iter().collect(),
            complete: store.complete_inventory().into_iter().collect(),
            heads: store.log().unwrap().heads(),
            stamp: fsx::stamp(&path).unwrap(),
        }
    }

    fn pass(&self, expected_post: Option<&[u8]>, measured: bool) -> usize {
        let before = self.snapshot();
        let traffic = self.host.traffic();
        let have = client::load_records(&self.paths).unwrap().have();
        if measured {
            GROSS.store(0, Ordering::SeqCst);
            assert!(!ACTIVE.swap(true, Ordering::SeqCst));
        }
        let counting = measured.then_some(Measuring);
        let result = client::test_sync_records_without_discovery(&self.paths);
        drop(counting);
        let gross = if measured {
            GROSS.load(Ordering::SeqCst)
        } else {
            0
        };
        assert_eq!(result.unwrap(), 0);
        assert!(
            self.snapshot() == before,
            "sync changed the local record file or Store"
        );
        let after = self.host.traffic();
        assert!(
            after.errors.is_empty(),
            "fixture errors: {:?}",
            after.errors
        );
        assert_eq!(after.gets.len(), traffic.gets.len() + 1);
        let request = after.gets.last().unwrap();
        let query = request
            .split_whitespace()
            .nth(1)
            .unwrap()
            .split_once('?')
            .unwrap()
            .1;
        let named = query
            .split('&')
            .find_map(|part| part.strip_prefix("have="))
            .unwrap();
        let expected_have = have
            .iter()
            .map(|id| {
                let mut text = String::new();
                for byte in id {
                    use std::fmt::Write as _;
                    write!(&mut text, "{byte:02x}").unwrap();
                }
                text
            })
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(named, expected_have);
        assert_eq!(
            after.posts.len(),
            traffic.posts.len() + usize::from(expected_post.is_some())
        );
        if let Some(expected) = expected_post {
            let posted = after.posts.last().unwrap();
            assert!(
                posted.exact,
                "upload differed from independently expected signed bytes"
            );
            assert_eq!(posted.length, expected.len());
            assert_eq!(posted.digest, Sha256::digest(expected));
        }
        gross
    }

    fn prime(&self, expected: &[u8]) {
        self.host.expected(expected);
        self.pass(Some(expected), false);
        self.pass(None, false);
    }
}

#[test]
fn ciphertext_sync_has_bounded_gross_allocations_for_none_whole_and_large_past() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = client::ServersAnswering::within(HANG);
    let mut measured = Vec::new();
    {
        let full = Fixture::new(false);
        full.prime(&full.history.full);
        measured.push(("none", full.pass(None, true)));
        full.host.state.lock().unwrap().boot = 2;
        full.host.expected(&full.history.full);
        measured.push(("whole", full.pass(Some(&full.history.full), true)));
        full.pass(None, false);
    }
    {
        let past = Fixture::new(true);
        past.prime(&past.history.prefix);
        assert_eq!(
            client::save_records(&past.paths, &past.history.tail).unwrap(),
            VALUES
        );
        let grown = client::load_records(&past.paths).unwrap();
        assert_eq!(
            grown.complete_inventory(),
            past.history.complete.complete_inventory()
        );
        assert_eq!(grown.len(), VALUES + 4);
        assert!(grown.complete_bytes(past.history.waiting.id()).is_none());
        let mut expected_file = past.history.full.clone();
        expected_file.extend_from_slice(past.history.waiting.bytes());
        assert!(grown.encode() == expected_file);
        assert!(client::load_records(&past.paths).unwrap().encode() == expected_file);
        past.host.expected(&past.history.tail);
        measured.push(("past", past.pass(Some(&past.history.tail), true)));
        past.pass(None, false);
    }
    for (case, gross) in &measured {
        println!("ciphertext sync case={case} gross_client_allocations={gross} bound={BOUND}");
    }
    assert!(
        measured.iter().all(|(_, gross)| *gross <= BOUND),
        "gross client allocation budget exceeded: {measured:?}"
    );
}
