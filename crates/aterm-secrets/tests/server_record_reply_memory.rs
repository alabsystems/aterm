// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual records GET handler streams replies without a second complete wire buffer.

#![cfg(unix)]

use aterm_digest::Sha256;
use aterm_secrets::{
    SecretBytes,
    age::Identity,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Body, Entrant, EpochAudience, Inserted, Log, MAX_RECORDS_STREAM, Record, RecordId, Store,
        encode_records,
    },
    server::{self, Options, Running},
    text::{Audience, Entry},
    transport::{NO_WAIT_HEADER, RECORDS_BOOT_HEADER, RECORDS_NEED_HEADER},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: pointer/layout pairs are forwarded unchanged to System. Constant TLS
// counters allocate nothing; default realloc counts both blocks during growth.
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

const NAME: &str = "record-reply-memory";
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const HANG: Duration = Duration::from_secs(60);

struct Fixture {
    running: Option<Running>,
    expected: Store,
    _directory: aterm_tempfile::TempDir,
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
        let mut wire = encode_records(log.records());
        assert!(wire.len() > VALUES * VALUE_BYTES);
        let parent = log
            .draft(
                &actor,
                100,
                Body::Name {
                    name: "parent".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        assert_eq!(log.insert(parent).unwrap(), Inserted::Held);
        let child = log
            .draft(
                &actor,
                101,
                Body::Name {
                    name: "waiting".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        wire.extend_from_slice(child.bytes());
        let keys = [envelope::signer_public(&signer).unwrap()];
        let expected = Store::for_vault(NAME)
            .unwrap()
            .under(&keys)
            .recovering(&wire);
        assert_eq!(expected.complete_inventory().len(), VALUES + 2);
        assert_eq!(expected.len(), VALUES + 3);
        assert!(expected.contains(child.id()));
        assert!(expected.complete_bytes(child.id()).is_none());
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
            pins: vec![(NAME.into(), keys[0])],
            hold: HANG,
            keep_awake: false,
        };
        let running = server::start(listener, &options).unwrap();
        let fixture = Self {
            running: Some(running),
            expected,
            _directory: directory,
        };
        let held = fixture.running().stored_records(NAME);
        assert_eq!(held.inventory(), fixture.expected.inventory());
        assert_eq!(
            held.complete_inventory(),
            fixture.expected.complete_inventory()
        );
        drop(held);
        fixture
    }

    fn running(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    fn expected(&self, have: &[RecordId]) -> Expected {
        let records = self.expected.missing(have);
        let wire = encode_records(&records);
        Expected {
            status: if wire.is_empty() { 204 } else { 200 },
            length: wire.len(),
            digest: Sha256::digest(&wire),
            need: have
                .iter()
                .any(|id| self.expected.complete_bytes(id).is_none()),
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

struct Expected {
    status: u16,
    length: usize,
    digest: [u8; 32],
    need: bool,
}

struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
    length: usize,
    digest: [u8; 32],
}

impl Response {
    fn check(&self, expected: &Expected, boot: Option<&str>) -> String {
        assert_eq!(self.status, expected.status);
        assert_eq!(self.length, expected.length);
        assert_eq!(self.digest, expected.digest);
        assert_eq!(
            self.headers.get("connection").map(String::as_str),
            Some("close")
        );
        assert_eq!(
            self.headers.get("cache-control").map(String::as_str),
            Some("no-store")
        );
        if expected.status == 204 {
            assert!(!self.headers.contains_key("content-length"));
            assert!(!self.headers.contains_key("content-type"));
        } else {
            assert_eq!(
                self.headers["content-length"].parse::<usize>().unwrap(),
                expected.length
            );
            assert_eq!(self.headers["content-type"], "application/octet-stream");
        }
        assert_eq!(
            self.headers
                .get(&RECORDS_NEED_HEADER.to_ascii_lowercase())
                .map(String::as_str),
            expected.need.then_some("1")
        );
        let actual_boot = &self.headers[&RECORDS_BOOT_HEADER.to_ascii_lowercase()];
        assert_eq!(actual_boot.len(), 32);
        assert!(actual_boot.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if let Some(boot) = boot {
            assert_eq!(actual_boot, boot);
        }
        actual_boot.clone()
    }
}

fn read_response(mut stream: TcpStream, request: String) -> Response {
    stream.set_read_timeout(Some(HANG)).unwrap();
    stream.set_write_timeout(Some(HANG)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
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
    let mut status_line = lines.next().unwrap().split_whitespace();
    assert_eq!(status_line.next(), Some("HTTP/1.1"));
    let status = status_line.next().unwrap().parse().unwrap();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').unwrap();
        assert!(
            headers
                .insert(name.to_ascii_lowercase(), value.trim().to_owned())
                .is_none()
        );
    }
    let mut hash = Sha256::new();
    let mut length = 0usize;
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        length += read;
        assert!(length <= MAX_RECORDS_STREAM);
        hash.update(&buffer[..read]);
    }
    Response {
        status,
        headers,
        length,
        digest: hash.finalize(),
    }
}

fn connected(have: &[RecordId]) -> (TcpStream, JoinHandle<Response>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect_timeout(&listener.local_addr().unwrap(), HANG).unwrap();
    let (server, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let query = if have.is_empty() {
        String::new()
    } else {
        format!(
            "?have={}",
            have.iter()
                .map(|id| aterm_codec::hex::encode_lower(id))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let request = format!(
        "GET /v1/{NAME}/records{query} HTTP/1.1\r\nHost: localhost\r\n{NO_WAIT_HEADER}: 1\r\nConnection: close\r\n\r\n"
    );
    let response = std::thread::spawn(move || read_response(client, request));
    (server, response)
}

fn exchange(fixture: &Fixture, have: &[RecordId]) -> Response {
    let (stream, response) = connected(have);
    fixture.running().test_handle_stream(stream).unwrap();
    response.join().unwrap()
}

#[test]
fn a_real_records_get_bounds_encoded_reply_allocation() {
    let fixture = Fixture::new();
    assert_eq!(fixture.expected.missing(&[]).len(), VALUES + 2);
    let expected = fixture.expected(&[]);
    assert!(expected.length > VALUES * VALUE_BYTES);
    let boot = exchange(&fixture, &[]).check(&expected, None);
    let (stream, response) = connected(&[]);
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    fixture.running().test_handle_stream(stream).unwrap();
    drop(measuring);
    let live = LIVE.with(Cell::get);
    let peak = PEAK.with(Cell::get);
    response.join().unwrap().check(&expected, Some(&boot));
    assert_eq!(live, 0, "all measured request/reply owners released");
    let bound = 512 * 1024;
    println!(
        "records GET records={} wire={} peak={peak} bound={bound}",
        VALUES + 2,
        expected.length
    );
    assert!(
        peak <= bound,
        "records GET reply peak {peak} exceeds {bound}"
    );
}

#[test]
fn records_get_keeps_have_need_boot_and_complete_only_semantics() {
    let fixture = Fixture::new();
    let boot = exchange(&fixture, &[]).check(&fixture.expected(&[]), None);
    let genesis = *fixture.expected.log().unwrap().network();
    let prefix = [genesis];
    let expected = fixture.expected(&prefix);
    assert_eq!(fixture.expected.missing(&prefix).len(), VALUES + 1);
    exchange(&fixture, &prefix).check(&expected, Some(&boot));
    let unknown = [[0x77; 32]];
    assert!(!fixture.expected.contains(&unknown[0]));
    let expected = fixture.expected(&unknown);
    assert!(expected.need);
    exchange(&fixture, &unknown).check(&expected, Some(&boot));
    let frontier = fixture.expected.frontier();
    let expected = fixture.expected(&frontier);
    assert_eq!(expected.status, 204);
    exchange(&fixture, &frontier).check(&expected, Some(&boot));
    let mut frontier_with_unknown = frontier;
    frontier_with_unknown.extend(unknown);
    let expected = fixture.expected(&frontier_with_unknown);
    assert_eq!(expected.status, 204);
    assert!(expected.need);
    exchange(&fixture, &frontier_with_unknown).check(&expected, Some(&boot));
}
