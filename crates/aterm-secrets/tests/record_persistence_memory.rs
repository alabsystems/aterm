// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A warm public record save does not allocate the whole persisted ciphertext stream.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes, client,
    commons::bootstrap::Bootstrap,
    fsx,
    membership_log::{Body, EpochAudience, Inserted, MemberId, Record, Store, encode_records},
    text::{Audience, Entry},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::os::unix::fs::MetadataExt;

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

const VALUES: usize = 12;
const VALUE_BYTES: usize = 512 * 1024;
const BOUND: usize = 512 * 1024;

struct Fixture {
    paths: client::Paths,
    delta: Record,
    actor: MemberId,
    initial: Store,
    expected: Store,
    expected_wire: Vec<u8>,
}

fn fixture(home: &std::path::Path) -> Fixture {
    let paths = client::Paths::for_home(&home.join("member"));
    let vault = home.join("team.vault");
    client::create(&paths, vault.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "seed-value", b"seed")
        .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    client::sync(&paths, None).unwrap();
    client::seed_vault_genesis(&paths, &home.join("genesis.alm")).unwrap();
    let bootstrap = Bootstrap::required(&paths).unwrap();
    let request = bootstrap.request().unwrap();
    let actor = request.binding().actor();
    let incarnation = *request.binding().incarnation();
    let mut log = client::load_records(&paths).unwrap().log().unwrap().clone();
    let config = client::config(&paths).unwrap().unwrap();
    assert!(!config.name.is_empty());
    let Body::Genesis { signer, root, .. } = log.genesis().body() else {
        panic!("the configured stream is rooted");
    };
    assert_eq!(config.signer, Some(*signer));
    assert_eq!(root.network, bootstrap.trust().network);
    assert_eq!(root.authority, bootstrap.trust().authority);
    assert!(log.membership().members().contains_key(&actor));
    let authored = log
        .records()
        .map(|record| record.authored_ms())
        .max()
        .unwrap()
        + 1;
    let generation = log
        .records()
        .filter_map(|record| match record.body() {
            Body::Epoch {
                audience: EpochAudience::Everyone,
                generation,
                ..
            } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    let sign = |draft: aterm_secrets::membership_log::Draft| {
        bootstrap
            .device()
            .with_seed(|seed| draft.sign(seed, incarnation))
            .unwrap()
    };
    let (draft, key) = log
        .draft_epoch(&actor, authored, EpochAudience::Everyone, generation)
        .unwrap();
    let epoch = sign(draft);
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    let wrap = log
        .draft_wrap(
            &actor,
            authored + 1,
            epoch_id,
            &key,
            request.binding().recipient(),
        )
        .unwrap();
    assert_eq!(log.insert(sign(wrap)).unwrap(), Inserted::Held);
    for i in 0..VALUES {
        let entry = Entry {
            name: format!("large-{i}"),
            value: SecretBytes::new(vec![i as u8; VALUE_BYTES]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let put = log
            .draft_put(&actor, authored + 2 + i as u64, epoch_id, &key, &entry)
            .unwrap();
        assert_eq!(log.insert(sign(put)).unwrap(), Inserted::Held);
    }
    let gone = Entry {
        name: "gone".into(),
        value: SecretBytes::new(b"removed".to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = log
        .draft_put(&actor, authored + 100, epoch_id, &key, &gone)
        .unwrap();
    assert_eq!(log.insert(sign(put)).unwrap(), Inserted::Held);
    let delete = log
        .draft_delete(&actor, authored + 101, epoch_id, &key, "gone")
        .unwrap();
    assert_eq!(log.insert(sign(delete)).unwrap(), Inserted::Held);
    let wire = encode_records(log.records());
    assert!(wire.len() > VALUES * VALUE_BYTES);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let initial = client::load_records(&paths).unwrap();
    assert_eq!(initial.encode(), wire);
    assert_eq!(initial.complete_inventory().len(), initial.len());
    assert_eq!(client::load_records(&paths).unwrap().encode(), wire);
    // Prime the actual state-lock/save path without adding or rewriting records.
    assert_eq!(
        client::save_records(&paths, log.genesis().bytes()).unwrap(),
        0
    );
    let delta = sign(log.draft(
        &actor,
        authored + 102,
        Body::Name {
            name: "after-save".into(),
        },
    ));
    assert!(!initial.contains(delta.id()));
    let mut expected = initial.clone();
    assert_eq!(expected.ingest(delta.bytes()).unwrap(), 1);
    let mut expected_wire = wire.clone();
    expected_wire.extend_from_slice(delta.bytes());
    assert_eq!(expected.encode(), expected_wire);
    assert!(expected_wire.len() > wire.len());
    assert_eq!(expected.complete_inventory().len(), expected.len());
    Fixture {
        paths,
        delta,
        actor,
        initial,
        expected,
        expected_wire,
    }
}

#[test]
fn a_warm_public_record_save_uses_bounded_persistence_scratch() {
    let home = aterm_tempfile::tempdir().unwrap();
    let f = fixture(home.path());
    let before_mode = std::fs::symlink_metadata(f.paths.records_file())
        .unwrap()
        .mode()
        & 0o777;
    assert_eq!(before_mode, 0o600);

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let added = client::save_records(&f.paths, f.delta.bytes()).unwrap();
    drop(measuring);
    let live = LIVE.with(Cell::get);
    let peak = PEAK.with(Cell::get);

    // File reads, new cache insertion and plaintext resolution occur only after counting.
    assert_eq!(added, 1);
    assert_eq!(live, 0, "all measured save owners released");
    let metadata = std::fs::symlink_metadata(f.paths.records_file()).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.mode() & 0o777, before_mode);
    assert_eq!(
        std::fs::read(f.paths.records_file()).unwrap(),
        f.expected_wire
    );
    let fresh = client::load_records(&f.paths).unwrap();
    assert_eq!(fresh.encode(), f.expected_wire);
    assert_eq!(fresh.len(), f.initial.len() + 1);
    assert_eq!(fresh.inventory(), f.expected.inventory());
    assert_eq!(fresh.complete_inventory(), f.expected.complete_inventory());
    for id in f.initial.inventory() {
        assert_eq!(fresh.get_bytes(&id), f.initial.get_bytes(&id));
    }
    let log = fresh.log().unwrap();
    assert_eq!(log.membership().members()[&f.actor].name, "after-save");
    let identity = client::read_machine_identity(&f.paths).unwrap().unwrap();
    let entries = log.open_entries(&identity).unwrap().unwrap();
    assert!(entries.iter().all(|entry| entry.name != "gone"));
    for i in 0..VALUES {
        let name = format!("large-{i}");
        let entry = entries.iter().find(|entry| entry.name == name).unwrap();
        assert_eq!(entry.value.len(), VALUE_BYTES);
        assert!(entry.value.as_slice().iter().all(|byte| *byte == i as u8));
    }
    println!(
        "public save records={} wire={} added={added} peak={peak} bound={BOUND}",
        fresh.len(),
        f.expected_wire.len()
    );
    assert!(
        peak <= BOUND,
        "public save persistence peak {peak} exceeds {BOUND}"
    );
}
