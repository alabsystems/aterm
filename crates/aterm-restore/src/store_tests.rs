// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Byte copies at every alignment land exactly, leave their neighbours alone, and
/// read back; words round-trip; every access outside the region is refused.
#[test]
fn the_heap_region_copies_and_bounds_like_the_mapping() {
    let store = VecStore::new(64);
    store.write(0, &[0xAA; 64]).expect("fill");
    for at in 0..16 {
        for len in 0..20 {
            let bytes: Vec<u8> = (0..len).map(|i| (at * 31 + i) as u8).collect();
            store.write(at, &bytes).expect("inside");
            let mut back = vec![0; len];
            store.read(at, &mut back).expect("inside");
            assert_eq!(back, bytes, "at {at} len {len}");
            let mut after = [0u8; 1];
            store.read(at + len, &mut after).expect("inside");
            assert_eq!(after[0], 0xAA, "the byte after {at}+{len} is untouched");
            store.write(at, &vec![0xAA; len]).expect("restore");
        }
    }
    store
        .store_u64_release(16, 0x0102_0304_0506_0708)
        .expect("aligned");
    assert_eq!(store.load_u64_acquire(16), Ok(0x0102_0304_0506_0708));
    let mut le = [0u8; 8];
    store.read(16, &mut le).expect("inside");
    assert_eq!(le, 0x0102_0304_0506_0708_u64.to_le_bytes(), "little-endian");
    assert_eq!(
        store.load_u64_acquire(12),
        Err(StoreError::Misaligned { at: 12 })
    );
    assert_eq!(
        store.load_u64_acquire(64),
        Err(StoreError::OutOfBounds { at: 64, len: 8 })
    );
    assert_eq!(
        store.write(60, &[0; 5]),
        Err(StoreError::OutOfBounds { at: 60, len: 5 })
    );
    assert_eq!(
        store.read(usize::MAX, &mut [0; 2]),
        Err(StoreError::OutOfBounds {
            at: usize::MAX,
            len: 2
        })
    );
}

/// A shared region takes a writer on one thread and another thread's word at the
/// same time, as the window's parse and gather threads write one session's region:
/// every store lands, and the session recovers from it as from an unshared one.
#[test]
fn a_shared_region_takes_two_threads_words() {
    use crate::layout::{Geometry, OFF_STREAM_GATHERED};
    use crate::recover::{RecoverPolicy, Recovery, recover};
    use crate::writer::{JournalWriter, KeyframeInput, WriterIdentity, store_gathered};
    use aterm_core::terminal::Terminal;

    let geometry = Geometry::v1();
    let shared = Shared::new(VecStore::new(geometry.region_len));
    let mut writer = JournalWriter::create(
        shared.clone(),
        geometry,
        &WriterIdentity {
            pid: 1,
            birth_us: 2,
            local_id: 3,
            build: 4,
            sid: "s-shared",
        },
    )
    .expect("created");
    let mut term = Terminal::new(6, 30);
    let checkpoint = term.checkpoint_journal(0).expect("ground");
    writer
        .write_keyframe(&KeyframeInput {
            checkpoint: &checkpoint,
            fence: term.history_fence(),
            resize_lifetime: term.resize_lifetime(),
            resize_ordinal: term.resize_ordinal(),
            bridge_lines: 0,
            config_fp: 9,
            integration_on: term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        })
        .expect("keyframe");
    let slices: Vec<Vec<u8>> = (0..200)
        .map(|i| format!("line {i}\r\n").into_bytes())
        .collect();
    let total: u64 = slices.iter().map(|s| s.len() as u64).sum();
    let gather = {
        let store = shared.clone();
        let counts: Vec<u64> = slices
            .iter()
            .scan(0u64, |n, s| {
                *n += s.len() as u64;
                Some(*n)
            })
            .collect();
        std::thread::spawn(move || {
            for n in counts {
                store_gathered(&store, n).expect("gathered");
            }
        })
    };
    for slice in &slices {
        term.process(slice);
        writer.append_slice(slice).expect("appended");
    }
    gather.join().expect("the gather thread");
    assert_eq!(shared.load_u64_acquire(OFF_STREAM_GATHERED), Ok(total));
    let dead = shared.with(VecStore::snapshot);
    let Recovery::Restored(restored) = recover(
        &dead,
        RecoverPolicy {
            tail: true,
            config_fp: 9,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    ) else {
        panic!("the shared region restores a screen");
    };
    assert!(restored.exact, "{:?}", restored.losses);
    assert_eq!(restored.replayed_records, 200);
}

/// A touched region counts each page once, at its first store, whatever wrote it and
/// however often; a refused store counts nothing; the meter carries every live region's
/// pages and takes a region's back when it is dropped.
#[test]
fn a_touched_region_counts_each_page_once_and_gives_them_back_when_dropped() {
    let meter = Arc::new(AtomicU64::new(0));
    let region = Touched::new(VecStore::new(4 * PAGE), Some(Arc::clone(&meter)));
    assert_eq!(
        region.touched_bytes(),
        0,
        "nothing written, nothing resident"
    );
    region.store_u64_release(0, 1).expect("a header word");
    assert_eq!(region.touched_bytes(), PAGE);
    region.write(8, &[7; 64]).expect("the same page");
    region
        .store_u64_release(16, 2)
        .expect("the same page again");
    assert_eq!(region.touched_bytes(), PAGE, "a page counts once");
    region
        .write(PAGE - 4, &[9; 8])
        .expect("a copy across the first boundary");
    assert_eq!(region.touched_bytes(), 2 * PAGE, "both pages it spans");
    assert!(region.touches(PAGE, 1) && !region.touches(2 * PAGE, PAGE));
    assert_eq!(
        region.write(4 * PAGE - 4, &[1; 8]),
        Err(StoreError::OutOfBounds {
            at: 4 * PAGE - 4,
            len: 8
        })
    );
    assert_eq!(
        region.touched_bytes(),
        2 * PAGE,
        "a refused store touches nothing"
    );
    region
        .store_u64_release(3 * PAGE + 8, 3)
        .expect("the last page");
    assert_eq!(region.touched_bytes(), 3 * PAGE);
    assert!(
        !region.touches(2 * PAGE, PAGE),
        "the page between, never written"
    );
    assert_eq!(meter.load(Ordering::Relaxed), 3 * PAGE as u64);
    let other = Touched::new(VecStore::new(2 * PAGE), Some(Arc::clone(&meter)));
    other.write(PAGE, &[1]).expect("one page");
    assert_eq!(
        meter.load(Ordering::Relaxed),
        4 * PAGE as u64,
        "every live region"
    );
    drop(region);
    assert_eq!(
        meter.load(Ordering::Relaxed),
        PAGE as u64,
        "a dropped region's pages leave the meter"
    );
    drop(other);
    assert_eq!(meter.load(Ordering::Relaxed), 0);
}

/// A replaced region takes every clone's next store, with what the seed stored already
/// in it; the region it replaced comes back as it was; a region of another length is
/// refused and the shared one left in place.
#[test]
fn a_replaced_region_takes_every_clone_s_next_store() {
    let shared = Shared::new(VecStore::new(64));
    let other_thread = shared.clone();
    shared
        .store_u64_release(0, 1)
        .expect("into the first region");
    let old = shared
        .swap_in(VecStore::new(64), |new| {
            new.store_u64_release(8, 2).expect("the seed");
        })
        .expect("same length");
    other_thread
        .store_u64_release(16, 3)
        .expect("a clone's next store");
    assert_eq!(shared.load_u64_acquire(0), Ok(0), "a new region");
    assert_eq!(shared.load_u64_acquire(8), Ok(2), "the seed's word");
    assert_eq!(other_thread.load_u64_acquire(16), Ok(3));
    assert_eq!(old.load_u64_acquire(0), Ok(1), "the old region as it was");
    assert_eq!(
        old.load_u64_acquire(16),
        Ok(0),
        "and nothing after the swap"
    );
    let refused = shared
        .swap_in(VecStore::new(128), |_| {
            panic!("no seed for a refused region")
        })
        .expect_err("another length");
    assert_eq!(refused.len(), 128);
    assert_eq!(
        shared.load_u64_acquire(8),
        Ok(2),
        "still the region in place"
    );
}

/// The countdown executes exactly its budget and drops the rest silently.
#[test]
fn the_countdown_executes_its_budget_and_drops_the_rest() {
    let store = CountdownStore::new(64, Some(2));
    store.store_u64_release(0, 1).expect("one");
    store.write(8, &[2; 8]).expect("two");
    assert!(!store.died());
    store.store_u64_release(16, 3).expect("dropped, silently");
    store.write(24, &[4; 8]).expect("dropped, silently");
    assert!(store.died());
    assert_eq!(store.stores(), 4);
    let dead = store.snapshot();
    assert_eq!(dead.load_u64_acquire(0), Ok(1));
    assert_eq!(dead.load_u64_acquire(8), Ok(u64::from_le_bytes([2; 8])));
    assert_eq!(dead.load_u64_acquire(16), Ok(0), "never stored");
    assert_eq!(dead.load_u64_acquire(24), Ok(0), "never copied");
}
