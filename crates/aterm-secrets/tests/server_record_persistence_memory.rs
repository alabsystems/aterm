// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Warm server persistence streams accepted records without a complete encoded copy.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes,
    age::Identity,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{Body, Entrant, EpochAudience, Inserted, Log, Record, RecordId, Store},
    server::{self, Options, Running},
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// One test at a time. A measured pass counts its own thread's bytes, but `aterm_uds::spawn_lock`'s
/// debug seam (`seam::Queued`) keeps a 64-byte mutex and a 32-byte list from the first wait on that
/// process-wide lock: another test's open beside a measured write read as 96 bytes that write
/// retained, in 18 of 40 runs of this binary.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: pointers and unchanged layouts are forwarded to System. Constant TLS
// counters allocate nothing. Default realloc counts both blocks during growth.
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

const NAME: &str = "server-record-persistence-memory";
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const BOUND: usize = 512 * 1024;
const WRITER: [u8; 32] = [17; 32];
const INCARNATION: [u8; 16] = [18; 16];
const SIGNER: [u8; 32] = [23; 32];

struct Fixture {
    running: Option<Running>,
    accepted: Store,
    identity: Identity,
    pending: RecordId,
    wire: Vec<u8>,
    prefix: Vec<u8>,
    path: PathBuf,
    keys: [[u8; 32]; 1],
    _directory: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = aterm_tempfile::tempdir().unwrap();
        let identity = Identity::from_secret_bytes([19; 32]);
        let request = Request::create(
            [20; 16],
            [21; 32],
            [22; 16],
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
                node_key: [24; 32],
                name: "reader".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let mut wire = genesis.bytes().to_vec();
        let mut log = Log::new(genesis).unwrap();
        let (draft, key) = log
            .draft_epoch(&actor, 2, EpochAudience::Everyone, 1)
            .unwrap();
        let epoch = draft.sign(&WRITER, INCARNATION).unwrap();
        let epoch_id = *epoch.id();
        wire.extend_from_slice(epoch.bytes());
        assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
        let prefix = wire.clone();
        let wrap = log
            .draft_wrap(&actor, 3, epoch_id, &key, identity.recipient())
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        wire.extend_from_slice(wrap.bytes());
        assert_eq!(log.insert(wrap).unwrap(), Inserted::Held);
        for i in 0..VALUES {
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
            wire.extend_from_slice(record.bytes());
            assert_eq!(log.insert(record).unwrap(), Inserted::Held);
        }
        assert_eq!(log.held_count(), VALUES + 3);
        assert!(wire.len() > VALUES * VALUE_BYTES);
        let keys = [envelope::signer_public(&SIGNER).unwrap()];
        let mut accepted = Store::for_vault(NAME)
            .unwrap()
            .under(&keys)
            .recovering(&wire);
        assert_eq!(accepted.complete_inventory().len(), VALUES + 3);
        assert_eq!(accepted.encode_held(), wire);

        // A real signed child waits on its absent parent. It must remain absent
        // from the server file even though the accepted Store owns it.
        let parent = log
            .draft(
                &actor,
                100,
                Body::Name {
                    name: "parent".into(),
                },
            )
            .sign(&WRITER, INCARNATION)
            .unwrap();
        assert_eq!(log.insert(parent).unwrap(), Inserted::Held);
        let child = log
            .draft(
                &actor,
                101,
                Body::Name {
                    name: "child".into(),
                },
            )
            .sign(&WRITER, INCARNATION)
            .unwrap();
        let pending = *child.id();
        assert!(accepted.insert(child).unwrap());
        assert_eq!(accepted.len(), VALUES + 4);
        assert!(accepted.complete_bytes(&pending).is_none());
        assert_ne!(
            accepted.encode(),
            wire,
            "waiting bytes distinguish local encoding"
        );
        assert_eq!(accepted.encode_held(), wire);

        let path = directory.path().join(format!("{NAME}.records"));
        fsx::write_atomic(&path, &prefix, 0o600).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let options = Options {
            listen: listener.local_addr().unwrap(),
            store: directory.path().to_path_buf(),
            advertise: None,
            pins: vec![(NAME.into(), keys[0])],
            hold: Duration::from_secs(60),
            keep_awake: false,
        };
        let running = server::start(listener, &options).unwrap();
        let fixture = Self {
            running: Some(running),
            accepted,
            identity,
            pending,
            wire,
            prefix,
            path,
            keys,
            _directory: directory,
        };
        let loaded = fixture.running().stored_records(NAME);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.encode_held(), fixture.prefix);
        assert_eq!(fixture.running().test_records_on_file(NAME), Some(2));
        fixture.assert_values(&fixture.accepted);
        fixture
    }

    fn running(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    fn assert_values(&self, store: &Store) {
        let entries = store
            .log()
            .unwrap()
            .open_entries(&self.identity)
            .unwrap()
            .unwrap();
        assert_eq!(entries.len(), VALUES);
        for i in 0..VALUES {
            let name = format!("value-{i}");
            let entry = entries.iter().find(|entry| entry.name == name).unwrap();
            assert_eq!(entry.audience, Audience::Everyone);
            assert_eq!(entry.value.len(), VALUE_BYTES);
            assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
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

fn measured(
    fixture: &Fixture,
    accepted: &Store,
    expected: fsx::FileStamp,
    on_file: Option<usize>,
) -> (Option<fsx::FileStamp>, usize, usize) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let result = fixture
        .running()
        .test_persist_records(NAME, accepted, Some(expected), on_file);
    let written = match result {
        Ok(written) => written,
        Err(error) => {
            drop(measuring);
            panic!("the production persistence helper failed: {error}");
        }
    };
    let live = LIVE.with(Cell::get);
    let peak = PEAK.with(Cell::get);
    drop(measuring);
    (written, live, peak)
}

fn exercise(known_prefix: bool) {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let fixture = Fixture::new();
    let prefix = Store::for_vault(NAME)
        .unwrap()
        .under(&fixture.keys)
        .recovering(&fixture.prefix);
    assert_eq!(prefix.len(), 2);
    let mut expected = fsx::stamp(&fixture.path).unwrap();
    // Initialize the real write mutex and I/O path on the existing prefix.
    // Darwin's mutex backing allocation belongs to Running, not a write.
    let warm = fixture
        .running()
        .test_persist_records(NAME, &prefix, Some(expected), Some(2))
        .unwrap()
        .unwrap();
    assert_eq!(fsx::stamp(&fixture.path), Some(warm));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), fixture.prefix);
    expected = warm;
    let on_file = known_prefix.then_some(2);
    for pass in 0..2 {
        let (written, live, peak) = measured(&fixture, &prefix, expected, on_file);
        assert_eq!(live, 0, "a repeated zero-tail write retained memory");
        expected = written.unwrap();
        assert_eq!(fsx::stamp(&fixture.path), Some(expected));
        assert_eq!(std::fs::read(&fixture.path).unwrap(), fixture.prefix);
        println!("zero-tail known_prefix={known_prefix} pass={pass} live={live} peak={peak}");
    }
    let before = fsx::stamp(&fixture.path).unwrap();
    assert_eq!(before.len, expected.len);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), fixture.prefix);
    assert_eq!(before, expected);

    // The accepted Store, all signed payload owners, expected output and warm
    // mutex backing exist before counting. Measure only helper-owned scratch.
    let (written, live, peak) = measured(&fixture, &fixture.accepted, expected, on_file);
    assert_eq!(live, 0, "all measured persistence owners released");

    let written = written.expect("the file still has this write's identity");
    assert_eq!(fsx::stamp(&fixture.path), Some(written));
    assert_eq!(written.len, fixture.wire.len() as u64);
    assert_eq!(
        (written.dev, written.ino) != (before.dev, before.ino),
        !known_prefix,
        "append retains the inode; an unknown prefix requires a whole replacement"
    );
    let metadata = std::fs::symlink_metadata(&fixture.path).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), fixture.wire);
    let (reloaded, on_file) =
        server::test_cold_records(&fixture.path, NAME, &fixture.keys).unwrap();
    assert_eq!(on_file, Some(VALUES + 3));
    assert_eq!(reloaded.len(), VALUES + 3);
    assert!(
        !reloaded.contains(&fixture.pending),
        "waiting child was not persisted"
    );
    assert_eq!(
        reloaded.complete_inventory(),
        fixture.accepted.complete_inventory()
    );
    assert_eq!(
        reloaded.log().unwrap().heads(),
        fixture.accepted.log().unwrap().heads()
    );
    for id in fixture.accepted.complete_inventory() {
        assert_eq!(reloaded.get_bytes(&id), fixture.accepted.get_bytes(&id));
    }
    fixture.assert_values(&reloaded);
    println!(
        "server persistence known_prefix={known_prefix} complete={} waiting=1 prefix={} wire={} tail={} peak={peak} bound={BOUND}",
        VALUES + 3,
        fixture.prefix.len(),
        fixture.wire.len(),
        fixture.wire.len() - fixture.prefix.len()
    );
    assert!(
        peak <= BOUND,
        "persistence heap peak {peak} exceeds {BOUND}"
    );
}

#[test]
fn a_large_accepted_tail_appends_with_bounded_persistence_scratch() {
    exercise(true);
}

#[test]
fn an_unknown_prefix_uses_a_bounded_whole_write() {
    exercise(false);
}
