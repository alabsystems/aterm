// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use std::sync::{Arc, Mutex};

fn assert_send<T: Send>() {}

/// Runtime check that a type does not implement `Sync`.
///
/// Inherent methods take priority over trait methods in method resolution.
/// When `T: Sync`, the inherent `is_sync_check()` resolves → returns `true`.
/// When `T: !Sync`, only the trait fallback is available → returns `false`.
/// The trait MUST be in scope at the call site for the fallback to resolve.
mod sync_check {
    pub struct Probe<T>(pub std::marker::PhantomData<T>);

    impl<T: Sync> Probe<T> {
        #[allow(dead_code)]
        pub fn is_sync_check(&self) -> bool {
            true
        }
    }

    pub trait NotSyncFallback {
        fn is_sync_check(&self) -> bool;
    }
    impl<T> NotSyncFallback for Probe<T> {
        fn is_sync_check(&self) -> bool {
            false
        }
    }
}

#[test]
fn scrollback_is_send_but_not_sync() {
    use sync_check::NotSyncFallback;

    // WarmTier uses Cell<u8> (decompress_failures), Cell<usize> (bytes_used),
    // and RefCell (block cache). All are Send but not Sync. This test codifies
    // the contract: Scrollback can move between threads but shared references
    // must go through external synchronization (Mutex).
    assert_send::<Scrollback>();

    // Verify !Sync via method resolution. If Scrollback were Sync, the
    // inherent method would resolve and return true, failing the assertion.
    let probe = sync_check::Probe::<Scrollback>(std::marker::PhantomData);
    assert!(
        !probe.is_sync_check(),
        "Scrollback must NOT be Sync — interior mutability (Cell, RefCell) \
         requires external synchronization for cross-thread sharing"
    );

    // Sanity: verify the probe returns true for a known Sync type.
    let sync_probe = sync_check::Probe::<i32>(std::marker::PhantomData);
    assert!(
        sync_probe.is_sync_check(),
        "i32 is Sync — probe should return true to validate the mechanism"
    );
}

#[test]
fn scrollback_moves_between_threads_when_guarded_by_mutex() {
    let scrollback = Arc::new(Mutex::new(Scrollback::with_block_size(3, 6, 10_000_000, 3)));
    {
        let mut sb = scrollback
            .lock()
            .expect("scrollback lock should not be poisoned");
        for idx in 0..9 {
            sb.push_str(&format!("Line {idx}"));
        }
    }

    let worker_scrollback = Arc::clone(&scrollback);
    let oldest_visible = std::thread::spawn(move || {
        let mut sb = worker_scrollback
            .lock()
            .expect("scrollback lock should not be poisoned");
        let oldest = sb
            .get_line(0)
            .expect("warm-tier read should succeed")
            .expect("oldest line should exist")
            .to_string();
        sb.push_str("worker line");
        oldest
    })
    .join()
    .expect("worker thread should finish cleanly");

    assert_eq!(oldest_visible, "Line 0");

    let sb = scrollback
        .lock()
        .expect("scrollback lock should not be poisoned");
    assert_eq!(
        sb.line_count(),
        10,
        "worker push should update shared state"
    );
    assert_eq!(
        sb.get_line(9)
            .expect("newest line lookup should succeed")
            .expect("worker line should exist")
            .to_string(),
        "worker line"
    );
}

/// Exercises the Cell<u8> decompress_failures path through Arc<Mutex>:
/// a worker thread corrupts a warm block and reads until quarantine.
/// This is the behavioral test for the #6024 Cell<u8> change — the
/// mutation through `&self` in `decompress()` must work correctly when
/// accessed through Mutex-guarded cross-thread sharing.
#[test]
fn cell_failure_counting_works_through_mutex_across_threads() {
    // hot=3, block_size=3: pushing 6 lines creates 1 warm block (lines 0-2)
    // and 3 hot lines (lines 3-5).
    let scrollback = Arc::new(Mutex::new(Scrollback::with_block_size(3, 6, 10_000_000, 3)));
    {
        let mut sb = scrollback
            .lock()
            .expect("scrollback lock should not be poisoned");
        for idx in 0..6 {
            sb.push_str(&format!("Line {idx}"));
        }
    }

    // Worker thread: verify warm read works, corrupt the block, then read
    // until the Cell<u8> counter reaches quarantine threshold.
    let worker_sb = Arc::clone(&scrollback);
    let quarantine_result = std::thread::spawn(move || {
        // Phase 1: verify pre-corruption reads work.
        {
            let sb = worker_sb
                .lock()
                .expect("scrollback lock should not be poisoned");
            let before = sb
                .get_line(0)
                .expect("pre-corruption read should succeed")
                .expect("warm line 0 should exist")
                .to_string();
            assert_eq!(before, "Line 0");
        }

        // Phase 2: corrupt the oldest warm block.
        {
            let mut sb = worker_sb
                .lock()
                .expect("scrollback lock should not be poisoned");
            sb.warm.corrupt_oldest_block();
        }

        // Phase 3: read the corrupted line repeatedly. Each get_line call
        // triggers decompress() which increments Cell<u8> on failure.
        let sb = worker_sb
            .lock()
            .expect("scrollback lock should not be poisoned");
        let mut decode_errors = 0u32;
        let mut quarantined = false;
        for _ in 0..10 {
            match sb.get_line(0) {
                Err(ScrollbackError::Quarantined(_)) => {
                    quarantined = true;
                    break;
                }
                Err(_) => decode_errors += 1,
                Ok(_) => panic!("corrupted block should not decompress successfully"),
            }
        }
        (decode_errors, quarantined)
    })
    .join()
    .expect("worker thread should finish cleanly");

    let (decode_errors, quarantined) = quarantine_result;
    assert_eq!(
        decode_errors,
        u32::from(crate::tier::QUARANTINE_THRESHOLD),
        "should see exactly QUARANTINE_THRESHOLD decode errors before quarantine"
    );
    assert!(quarantined, "block must reach quarantine state");

    // Main thread: verify the quarantine state persists through the mutex.
    let sb = scrollback
        .lock()
        .expect("scrollback lock should not be poisoned");
    let err = sb
        .get_line(0)
        .expect_err("quarantined block should remain quarantined");
    assert!(
        matches!(err, ScrollbackError::Quarantined(_)),
        "quarantine should persist across lock/unlock cycles"
    );
}
