// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A real duplicate records POST does not retain a second full request body.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes,
    age::Identity,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Entrant, EpochAudience, Inserted, Log, MAX_RECORD, Record, RecordId, encode_records,
    },
    server::{self, Options, Running},
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every pointer/layout pair is forwarded unchanged to System. Constant
// TLS counters allocate nothing; default realloc counts both blocks during growth.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forward the caller's valid layout unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = ACTIVE.try_with(|active| {
                if active.get() {
                    let _ = LIVE.try_with(|live| {
                        let next = live.get() + layout.size();
                        live.set(next);
                        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(next)));
                    });
                }
            });
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = LIVE.try_with(|live| live.set(live.get() - layout.size()));
            }
        });
        // SAFETY: forward the matching pointer and layout unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measuring;

impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

const NAME: &str = "record-post-memory";
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const BOUND: usize = 2 * MAX_RECORD + 1024 * 1024;
const HANG: Duration = Duration::from_secs(60);

struct Fixture {
    running: Option<Running>,
    wire: Arc<Vec<u8>>,
    directory: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = aterm_tempfile::tempdir().unwrap();
        let writer = [17; 32];
        let incarnation = [18; 16];
        let identity = Identity::from_secret_bytes([19; 32]);
        let request = Request::create(
            [20; 16],
            [21; 32],
            [22; 16],
            incarnation,
            &identity,
            &writer,
        )
        .unwrap();
        let actor = request.binding().actor();
        let signer = [23; 32];
        let genesis = Record::found(
            &signer,
            vec![Entrant {
                request,
                node_key: [24; 32],
                name: "reader".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let mut log = Log::new(genesis).unwrap();
        let (draft, key) = log
            .draft_epoch(&actor, 2, EpochAudience::Everyone, 1)
            .unwrap();
        let epoch = draft.sign(&writer, incarnation).unwrap();
        let epoch_id = *epoch.id();
        assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
        for i in 0..VALUES {
            let entry = Entry {
                name: format!("value-{i}"),
                value: SecretBytes::new(vec![i as u8; VALUE_BYTES]),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            };
            let record = log
                .draft_put(&actor, 3 + i as u64, epoch_id, &key, &entry)
                .unwrap()
                .sign(&writer, incarnation)
                .unwrap();
            assert_eq!(log.insert(record).unwrap(), Inserted::Held);
        }
        let wire = encode_records(log.records());
        assert!(wire.len() > VALUES * VALUE_BYTES);
        fsx::write_atomic(
            &directory.path().join(format!("{NAME}.records")),
            &wire,
            0o600,
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let options = Options {
            listen: listener.local_addr().unwrap(),
            store: directory.path().to_path_buf(),
            advertise: None,
            pins: vec![(NAME.into(), envelope::signer_public(&signer).unwrap())],
            hold: HANG,
            keep_awake: false,
        };
        let running = server::start(listener, &options).unwrap();
        let fixture = Self {
            running: Some(running),
            wire: Arc::new(wire),
            directory,
        };
        let loaded = fixture.running().stored_records(NAME);
        assert_eq!(loaded.len(), VALUES + 2);
        assert_eq!(loaded.complete_inventory().len(), loaded.len());
        assert_eq!(loaded.encode(), *fixture.wire);
        assert_eq!(
            fixture.running().test_records_on_file(NAME),
            Some(VALUES + 2)
        );
        fixture
    }

    fn running(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    fn snapshot(&self) -> Snapshot {
        let store = self.running().stored_records(NAME);
        let path = self.directory.path().join(format!("{NAME}.records"));
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_file());
        Snapshot {
            inventory: store.inventory(),
            complete: store.complete_inventory(),
            heads: store.log().unwrap().heads(),
            on_file: self.running().test_records_on_file(NAME),
            bytes: std::fs::read(path).unwrap(),
            metadata: (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mode(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    inventory: BTreeSet<RecordId>,
    complete: BTreeSet<RecordId>,
    heads: Vec<RecordId>,
    on_file: Option<usize>,
    bytes: Vec<u8>,
    metadata: (u64, u64, u64, u32, i64, i64),
}

fn post_client(mut stream: TcpStream, wire: Arc<Vec<u8>>) {
    stream.set_read_timeout(Some(HANG)).unwrap();
    stream.set_write_timeout(Some(HANG)).unwrap();
    write!(stream, "POST /v1/{NAME}/records HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", wire.len()).unwrap();
    stream.write_all(wire.as_slice()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < server::MAX_HEAD);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = std::str::from_utf8(&head).unwrap();
    let mut lines = head.split("\r\n");
    assert_eq!(lines.next(), Some("HTTP/1.1 204 No Content"));
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').unwrap();
        assert!(
            headers
                .insert(name.to_ascii_lowercase(), value.trim())
                .is_none()
        );
    }
    assert_eq!(headers.get("connection"), Some(&"close"));
    assert_eq!(headers.get("cache-control"), Some(&"no-store"));
    assert!(!headers.contains_key("content-length"));
    assert!(!headers.contains_key("content-type"));
    let mut body = [0; 4096];
    assert_eq!(stream.read(&mut body).unwrap(), 0, "204 must have no body");
}

fn connected(fixture: &Fixture) -> (TcpStream, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect_timeout(&listener.local_addr().unwrap(), HANG).unwrap();
    let (server, _) = listener.accept().unwrap();
    let wire = Arc::clone(&fixture.wire);
    let reader = std::thread::spawn(move || post_client(client, wire));
    (server, reader)
}

#[test]
fn a_real_duplicate_records_post_uses_bounded_input_scratch() {
    let fixture = Fixture::new();
    let (warm, client) = connected(&fixture);
    let handled = fixture.running().test_handle_stream(warm);
    let received = client.join();
    handled.unwrap();
    received.unwrap();
    let before = fixture.snapshot();
    assert_eq!(before.on_file, Some(VALUES + 2));
    assert_eq!(before.bytes, *fixture.wire);
    let (stream, client) = connected(&fixture);

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let handled = fixture.running().test_handle_stream(stream);
    drop(measuring);
    let live = LIVE.with(Cell::get);
    let peak = PEAK.with(Cell::get);
    let received = client.join();
    handled.unwrap();
    received.unwrap();
    assert_eq!(live, 0, "all measured POST owners released");
    assert!(
        fixture.snapshot() == before,
        "a duplicate POST changed retained state or file"
    );
    println!(
        "duplicate POST records={} wire={} peak={peak} bound={BOUND}",
        VALUES + 2,
        fixture.wire.len()
    );
    assert!(
        peak <= BOUND,
        "duplicate POST heap peak {peak} exceeds {BOUND}"
    );
}
