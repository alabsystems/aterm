// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `MainThreadBound<T>`: the half of the contract libtest can hold.
//!
//! Dropping a `MainThreadBound<T>` whose `T` needs a destructor on a libtest worker HANGS THE
//! WHOLE TEST BINARY: `Drop` reschedules onto the main queue ([`aterm_objc::run_on_main`]'s first
//! hang), and libtest's main thread is parked joining workers. So every test here holds a `T` with
//! no destructor or consumes the container with `into_inner` before it can drop; a test that
//! forgets this hangs instead of failing. That the destructor runs on the main thread is held by
//! `tests/main_thread_bound_drive.rs`, a `harness = false` test that drives a run loop.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicU32, Ordering};

use aterm_objc::{MainThread, MainThreadBound};

fn witness() -> MainThread {
    // SAFETY: Every `T` here is a plain Rust value with no AppKit state, droppable on any thread.
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
    drop(value);
    assert_eq!(1, DROPS.load(Ordering::Relaxed), "exactly once");
}
