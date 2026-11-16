// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold server loading retains verified records without retaining a whole input buffer too.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes,
    age::Identity,
    commons::binding::Request,
    envelope, fsx,
    membership_log::{
        Body, Entrant, EpochAudience, Inserted, Log, MAX_RECORD, MemberId, Record, Store,
        read_records,
    },
    server,
    text::{Audience, Entry},
};
use std::cell::Cell;

#[path = "memory_alloc/net_peak.rs"]
mod net_peak;

use net_peak::{ACTIVE, LIVE, PEAK};

struct Measuring;

impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

const NAME: &str = "cold-record-memory";
const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const WRITER: [u8; 32] = [17; 32];
const INCARNATION: [u8; 16] = [18; 16];
const GENESIS_SIGNER: [u8; 32] = [23; 32];

struct Fixture {
    log: Log,
    actor: MemberId,
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
            .sign(&WRITER, INCARNATION)
            .unwrap();
        assert_eq!(log.insert(record).unwrap(), Inserted::Held);
    }
    Fixture { log, actor }
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
        assert_eq!(
            actual.record(&id).map(|r| r.wire_bytes().unwrap()),
            expected.record(&id).map(|r| r.wire_bytes().unwrap())
        );
    }
}

#[test]
fn a_cold_server_load_bounds_input_scratch_beside_retained_records() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let path = directory.path().join("cold.records");
    let fixture = large_history();
    let wire = read_records(fixture.log.records()).unwrap();
    let keys = [envelope::signer_public(&GENESIS_SIGNER).unwrap()];
    let expected = Store::for_vault(NAME)
        .unwrap()
        .under(&keys)
        .recovering(&wire);
    assert_eq!(expected.len(), VALUES + 2);
    assert_eq!(expected.complete_inventory().len(), expected.len());
    assert_eq!(expected.read_encoded().unwrap(), wire);
    assert!(wire.len() > VALUES * VALUE_BYTES);
    fsx::write_atomic(&path, &wire, 0o600).unwrap();

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    // The seam creates a fresh server State and calls the production cold loader once.
    // It moves the actual slot's Store out: no background thread or warm slot is involved.
    let (observed, on_file) = server::test_cold_records(&path, NAME, &keys).unwrap();
    assert_same_records(&observed, &expected);
    assert_eq!(on_file, Some(expected.len()));
    let retained = LIVE.with(Cell::get);
    assert!(
        retained >= VALUES * VALUE_BYTES,
        "the measured Store retains each Put's signed ciphertext"
    );
    drop(observed);
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners released");
    let peak = PEAK.with(Cell::get);
    // A decoded body shares its record's wire. Bound that retained wire and metadata,
    // then the current input frame and signature-message scratch. A whole-input copy
    // does not fit the remaining allowance for this signed fixture.
    let retained_bound = wire.len() + 1024 * 1024;
    let bound = wire.len() + 2 * MAX_RECORD + 1024 * 1024;
    println!(
        "cold server wire={} records={} retained={retained} peak={peak} bound={bound}",
        wire.len(),
        expected.len()
    );
    assert!(
        retained <= retained_bound,
        "cold server retained {retained} exceeds {retained_bound}"
    );
    assert!(peak <= bound, "cold server peak {peak} exceeds {bound}");
}

#[test]
fn a_cold_server_load_preserves_waiting_and_torn_tail_recovery() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let path = directory.path().join("cold.records");
    let mut fixture = large_history();
    let wire = read_records(fixture.log.records()).unwrap();
    let keys = [envelope::signer_public(&GENESIS_SIGNER).unwrap()];
    let parent = fixture
        .log
        .draft(
            &fixture.actor,
            100,
            Body::Name {
                name: "parent".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    assert_eq!(fixture.log.insert(parent).unwrap(), Inserted::Held);
    let child = fixture
        .log
        .draft(
            &fixture.actor,
            101,
            Body::Name {
                name: "child".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    let mut pending = wire.clone();
    pending.extend_from_slice(child.wire_bytes().unwrap().as_ref());
    let mut torn = pending.clone();
    torn.extend_from_slice(b"ALM");
    for bytes in [&pending, &torn] {
        let expected = Store::for_vault(NAME)
            .unwrap()
            .under(&keys)
            .recovering(bytes);
        assert_eq!(expected.complete_inventory().len(), VALUES + 2);
        assert_eq!(expected.len(), VALUES + 3);
        assert!(expected.contains(child.id()));
        assert!(!expected.holds(child.id()));
        fsx::write_atomic(&path, bytes, 0o600).unwrap();
        let (observed, on_file) = server::test_cold_records(&path, NAME, &keys).unwrap();
        assert_same_records(&observed, &expected);
        assert_eq!(
            observed.read_encoded().unwrap(),
            expected.read_encoded().unwrap()
        );
        assert_eq!(on_file, None, "the complete prefix does not cover the file");
    }
}
