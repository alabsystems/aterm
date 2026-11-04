// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Decoded record payloads retain one wire owner and survive their original owners.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes,
    age::Identity,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Body, Entrant, EpochAudience, Inserted, Kind, Log, MAX_RECORD, Record, Store,
        encode_records,
    },
    server,
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every pointer and unchanged layout is forwarded to System. Constant TLS
// counters allocate nothing. The default realloc counts both blocks during growth.
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

const NAME: &str = "record-payload-memory";
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const WRITER: [u8; 32] = [17; 32];
const INCARNATION: [u8; 16] = [18; 16];
const GENESIS_SIGNER: [u8; 32] = [23; 32];

struct Fixture {
    log: Log,
    identity: Identity,
}

fn large_history() -> Fixture {
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
        &GENESIS_SIGNER,
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
    let epoch = draft.sign(&WRITER, INCARNATION).unwrap();
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    let wrap = log
        .draft_wrap(&actor, 3, epoch_id, &key, identity.recipient())
        .unwrap();
    assert_eq!(
        log.insert(wrap.sign(&WRITER, INCARNATION).unwrap())
            .unwrap(),
        Inserted::Held
    );
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
        assert_eq!(log.insert(record).unwrap(), Inserted::Held);
    }
    let gone = Entry {
        name: "gone".into(),
        value: SecretBytes::new(b"deleted value".to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = log.draft_put(&actor, 100, epoch_id, &key, &gone).unwrap();
    assert_eq!(
        log.insert(put.sign(&WRITER, INCARNATION).unwrap()).unwrap(),
        Inserted::Held
    );
    let delete = log
        .draft_delete(&actor, 101, epoch_id, &key, "gone")
        .unwrap();
    assert_eq!(
        log.insert(delete.sign(&WRITER, INCARNATION).unwrap())
            .unwrap(),
        Inserted::Held
    );
    Fixture { log, identity }
}

fn payload(body: &Body) -> Option<&[u8]> {
    match body {
        Body::Wrap { capsule, .. } => Some(capsule.as_ref()),
        Body::Put { ciphertext, .. } | Body::Delete { ciphertext, .. } => Some(ciphertext.as_ref()),
        _ => None,
    }
}

fn assert_same_records(actual: &Store, expected: &Store) {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.inventory(), expected.inventory());
    assert_eq!(actual.complete_inventory(), expected.complete_inventory());
    assert_eq!(
        actual.log().unwrap().heads(),
        expected.log().unwrap().heads()
    );
    for id in expected.inventory() {
        assert_eq!(actual.get_bytes(&id), expected.get_bytes(&id));
    }
}

#[test]
fn a_cold_store_retains_one_payload_owner_per_signed_record() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let path = directory.path().join("cold.records");
    let fixture = large_history();
    let wire = encode_records(fixture.log.records());
    let keys = [envelope::signer_public(&GENESIS_SIGNER).unwrap()];
    let expected = Store::for_vault(NAME)
        .unwrap()
        .under(&keys)
        .recovering(&wire);
    assert_eq!(expected.len(), VALUES + 5);
    assert_eq!(expected.complete_inventory().len(), expected.len());
    assert_eq!(expected.encode(), wire);
    assert!(wire.len() > VALUES * VALUE_BYTES);
    fsx::write_atomic(&path, &wire, 0o600).unwrap();

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let (observed, on_file) = server::test_cold_records(&path, NAME, &keys).unwrap();
    assert_same_records(&observed, &expected);
    assert_eq!(on_file, Some(expected.len()));
    let retained = LIVE.with(Cell::get);
    let cloned_store = observed.clone();
    drop(observed);
    assert_same_records(&cloned_store, &expected);
    let escaped: Vec<_> = [Kind::Wrap, Kind::Put, Kind::Delete]
        .into_iter()
        .map(|kind| {
            let record = cloned_store
                .log()
                .unwrap()
                .records()
                .find(|r| r.kind() == kind)
                .unwrap();
            (*record.id(), record.clone(), record.body().clone())
        })
        .collect();
    drop(cloned_store);
    for (id, record, body) in escaped {
        let original = fixture.log.get(&id).unwrap();
        assert_eq!(record.bytes(), original.bytes());
        assert_eq!(payload(record.body()), payload(original.body()));
        drop(record);
        assert_eq!(payload(&body), payload(original.body()));
    }
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners released");
    let peak = PEAK.with(Cell::get);
    // One retained signed wire plus small record/index metadata. Cold decoding may
    // also hold its input frame and signature-message scratch concurrently.
    let retained_bound = wire.len() + 1024 * 1024;
    let peak_bound = wire.len() + 2 * MAX_RECORD + 1024 * 1024;
    println!(
        "payload wire={} records={} retained={retained} retained_bound={retained_bound} peak={peak} peak_bound={peak_bound}",
        wire.len(),
        expected.len()
    );
    assert!(
        retained <= retained_bound,
        "retained payload bytes {retained} exceed {retained_bound}"
    );
    assert!(
        peak <= peak_bound,
        "payload peak {peak} exceeds {peak_bound}"
    );
}

#[test]
fn payload_clones_preserve_bytes_and_decrypt_after_owner_drops() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let path = directory.path().join("cold.records");
    let fixture = large_history();
    let wire = encode_records(fixture.log.records());
    let keys = [envelope::signer_public(&GENESIS_SIGNER).unwrap()];
    fsx::write_atomic(&path, &wire, 0o600).unwrap();
    let (observed, on_file) = server::test_cold_records(&path, NAME, &keys).unwrap();
    assert_eq!(on_file, Some(VALUES + 5));
    let cloned = observed.clone();
    drop(observed);
    let entries = cloned
        .log()
        .unwrap()
        .open_entries(&fixture.identity)
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), VALUES);
    assert!(entries.iter().all(|entry| entry.name != "gone"));
    for i in 0..VALUES {
        let name = format!("value-{i}");
        let entry = entries.iter().find(|entry| entry.name == name).unwrap();
        assert_eq!(entry.value.len(), VALUE_BYTES);
        assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
    }
    drop(entries);
    drop(cloned);
    for kind in [Kind::Wrap, Kind::Put, Kind::Delete] {
        let original = fixture
            .log
            .records()
            .find(|record| record.kind() == kind)
            .unwrap();
        let mut bytes = original.bytes().to_vec();
        let decoded = Record::decode(&bytes).unwrap();
        let record = decoded.clone();
        let body = decoded.body().clone();
        bytes.fill(0);
        drop(bytes);
        drop(decoded);
        assert_eq!(record.bytes(), original.bytes());
        assert_eq!(payload(record.body()), payload(original.body()));
        drop(record);
        assert_eq!(payload(&body), payload(original.body()));
    }
}
