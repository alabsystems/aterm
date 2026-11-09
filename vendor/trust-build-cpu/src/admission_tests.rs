// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

use crate::{Owner, Pool, SlotState};

fn width(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

#[test]
fn shrinking_pauses_new_admission_without_revoking_workers_and_recovers() {
    let _fixture = crate::tests::kernel_fixture();
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open_in(dir.path(), width(6)).unwrap();
    assert!(pool.try_acquire_with_limit(Owner::current(None), 0).unwrap().is_none());
    let mut held: Vec<_> = (0..4)
        .map(|_| pool.try_acquire_with_limit(Owner::current(None), 6).unwrap().unwrap())
        .collect();
    let slots: Vec<_> = held.iter().map(crate::Lease::slot).collect();
    assert!(pool.try_acquire_with_limit(Owner::current(None), 2).unwrap().is_none());
    assert_eq!(
        pool.snapshot().unwrap().iter().filter(|s| matches!(s, SlotState::Held { .. })).count(),
        4
    );
    assert_eq!(slots, held.iter().map(crate::Lease::slot).collect::<Vec<_>>());
    held.truncate(2);
    assert!(pool.try_acquire_with_limit(Owner::current(None), 2).unwrap().is_none());
    held.truncate(1);
    held.push(pool.try_acquire_with_limit(Owner::current(None), 2).unwrap().unwrap());
    held.push(pool.try_acquire_with_limit(Owner::current(None), 6).unwrap().unwrap());
    assert_eq!(held.len(), 3);
}

#[test]
fn competing_roots_cannot_race_past_the_measured_limit() {
    competing_roots_started(Instant::now());
}

#[test]
fn required_competing_roots_keep_the_original_start_and_actual_native_capacity() {
    competing_roots_started(Instant::now() - Duration::from_secs(1));
}

const ADMISSION_WAIT: Duration = Duration::from_secs(60);

fn competing_roots_started(started: Instant) {
    let _fixture = crate::tests::kernel_fixture();
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open_in(dir.path(), width(16)).unwrap();
    let ready = Arc::new(Barrier::new(16));
    let granted = Arc::new(AtomicUsize::new(0));
    let deadline = started + ADMISSION_WAIT;
    let (tx, rx) = mpsc::channel();
    let threads: Vec<_> = (0..16)
        .map(|_| {
            // Separate open descriptions, as in independent compiler roots.
            let pool = Pool::open_in(dir.path(), width(16)).unwrap();
            let ready = Arc::clone(&ready);
            let granted = Arc::clone(&granted);
            let tx = tx.clone();
            std::thread::spawn(move || {
                ready.wait();
                loop {
                    assert!(Instant::now() < deadline, "native admission did not complete");
                    if let Some(lease) =
                        pool.try_acquire_with_limit(Owner::current(None), 3).unwrap()
                    {
                        tx.send(Some(lease)).unwrap();
                        granted.fetch_add(1, Ordering::Release);
                        return;
                    }
                    if granted.load(Ordering::Acquire) >= 3 {
                        tx.send(None).unwrap();
                        return;
                    }
                    std::thread::yield_now();
                }
            })
        })
        .collect();
    drop(tx);
    // Retain successful leases until every contender has finished.
    let held: Vec<_> = rx.into_iter().flatten().collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(held.len(), 3);
    assert!(pool.try_acquire_with_limit(Owner::current(None), 3).unwrap().is_none());
    assert_eq!(
        pool.snapshot().unwrap().iter().filter(|s| matches!(s, SlotState::Held { .. })).count(),
        3
    );
}

#[test]
fn trusted_inherited_continuation_reuses_credit_while_unrelated_held_open_refuses() {
    let _fixture = crate::tests::kernel_fixture();
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open_in(dir.path(), width(1)).unwrap();
    let original = pool.try_acquire(Owner::current(None)).unwrap().unwrap();
    let unrelated = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pool.path().join("slot-00000000"))
        .unwrap();
    assert!(pool.adopt_inherited(unrelated).is_err());
    let inherited = pool.adopt_inherited(original.try_clone().unwrap().into_file()).unwrap();
    drop(original);
    assert!(pool.try_acquire(Owner::current(None)).unwrap().is_none());
    drop(inherited);
    let stale = std::fs::File::open(pool.path().join("slot-00000000")).unwrap();
    assert!(pool.adopt_inherited(stale).is_err());
    assert!(pool.try_acquire(Owner::current(None)).unwrap().is_some());
}

#[test]
fn busy_inheritance_keeps_the_original_descriptor_and_policy_veto_is_nonmutating() {
    let _fixture = crate::tests::kernel_fixture();
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open_in(dir.path(), width(2)).unwrap();
    let original = pool.try_acquire(Owner::current(None)).unwrap().unwrap();
    let file = original.try_clone().unwrap().into_file();
    let lock = std::fs::File::open(pool.path().join("pool")).unwrap();
    lock.lock().unwrap();
    assert!(pool.try_adopt_inherited(&file, |_| 2).unwrap().is_none());
    drop(lock);
    assert!(pool.try_adopt_inherited(&file, |_| 0).unwrap().is_none());
    assert!(pool.adopt_inherited_until(&file, std::time::Instant::now()).is_err());
    drop(original);
    assert_eq!(
        pool.snapshot().unwrap().iter().filter(|s| matches!(s, SlotState::Held { .. })).count(),
        1
    );
    drop(file);
    assert!(pool.snapshot().unwrap().iter().all(|s| matches!(s, SlotState::Available { .. })));
}
