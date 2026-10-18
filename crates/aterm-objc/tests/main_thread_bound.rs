// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `MainThreadBound<T>` — the half of the contract libtest can hold.
//!
//! # WHAT THIS FILE MUST NOT DO, AND WHY IT IS THE FIRST THING WRITTEN HERE
//!
//! **Dropping a `MainThreadBound<T>` where `T` needs a destructor, on a libtest
//! worker thread, HANGS THE WHOLE TEST BINARY.** That is not a defect in the
//! type; it is [`aterm_objc::run_on_main`]'s documented first hang arriving
//! through `Drop`. A libtest binary's main thread is parked joining workers, so
//! it never services its main queue, and the reschedule waits for ever.
//!
//! So every test below either holds a `T` with no destructor, or consumes the
//! container with `into_inner` before it can be dropped. A future test that
//! forgets this will not fail — it will hang.
//!
//! The half that CANNOT be held here — that the destructor really does run on
//! the main thread — is in `tests/main_thread_bound_drive.rs`, a
//! `harness = false` test that owns `fn main` and can drive a run loop.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicU32, Ordering};

use aterm_objc::{MainThread, MainThreadBound};

/// A witness for a value with NO main-thread affinity.
///
/// # Safety
/// Every `T` this file puts in a container is a plain Rust value that touches
/// no AppKit state and whose destructor (where it has one) is a counter
/// increment, so `MainThread::new_unchecked`'s SECOND form is what discharges
/// this: the class being handled has no main-thread affinity and its payload
/// may be dropped on whatever thread performs the last release.
fn witness() -> MainThread {
    // SAFETY: as documented above.
    unsafe { MainThread::new_unchecked() }
}

#[test]
fn into_inner_hands_the_value_back_and_does_not_run_the_containers_drop() {
    static DROPS: AtomicU32 = AtomicU32::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    DROPS.store(0, Ordering::Relaxed);
    let bound = MainThreadBound::new(Counted, witness());
    let value = bound.into_inner(witness());
    assert_eq!(
        0,
        DROPS.load(Ordering::Relaxed),
        "extracting must not run the destructor — and must not reschedule one either, which on \
         this thread would hang"
    );
    // Dropped HERE, as a bare `Counted`, on this worker: out of the container,
    // the value has no affinity and no reschedule.
    drop(value);
    assert_eq!(1, DROPS.load(Ordering::Relaxed), "exactly once");
}
