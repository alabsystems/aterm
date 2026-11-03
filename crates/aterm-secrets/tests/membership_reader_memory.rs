// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A warm membership load reads current bytes without allocating the whole stream again.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes,
    age::Identity,
    client::{Paths, load_records},
    commons::binding::Request,
    fsx,
    membership_log::{Entrant, EpochAudience, Inserted, Log, Record, encode_records},
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

fn large_history() -> Log {
    let seed = [17; 32];
    let incarnation = [18; 16];
    let identity = Identity::from_secret_bytes([19; 32]);
    let request =
        Request::create([20; 16], [21; 32], [22; 16], incarnation, &identity, &seed).unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[23; 32],
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
    let epoch = draft.sign(&seed, incarnation).unwrap();
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    for i in 0..6 {
        let entry = Entry {
            name: format!("value-{i}"),
            value: SecretBytes::new(vec![i as u8; 512 * 1024]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let record = log
            .draft_put(&actor, 3 + i, epoch_id, &key, &entry)
            .unwrap()
            .sign(&seed, incarnation)
            .unwrap();
        assert_eq!(log.insert(record).unwrap(), Inserted::Held);
    }
    log
}

#[test]
fn a_warm_membership_load_uses_bounded_stream_scratch() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let log = large_history();
    let wire = encode_records(log.records());
    assert!(wire.len() > 3 * 1024 * 1024);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let warm = load_records(&paths).unwrap();
    assert_eq!(warm.complete_inventory().len(), log.held_count());
    assert_eq!(warm.encode(), wire);
    drop(warm);

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let observed = load_records(&paths);
    let count = observed.as_ref().unwrap().len();
    drop(observed);
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners released");
    assert_eq!(count, log.held_count());
    let peak = PEAK.with(Cell::get);
    let bound = 256 * 1024;
    assert!(
        peak <= bound,
        "warm membership read peak {peak} exceeds {bound}"
    );
    println!(
        "warm membership bytes={} records={count} peak={peak} bound={bound}",
        wire.len()
    );
}
