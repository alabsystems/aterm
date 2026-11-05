// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a creation that waits on the spawn lock leaves on its thread's heap, in a test process of
//! its own: its one test holds the lock from one thread while another waits on it, under a global
//! allocator that counts each thread's live bytes while that thread asks it to.
#![cfg(target_vendor = "apple")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_uds::spawn_lock::seam;

/// A hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every call forwards its pointer and layout unchanged to `System`; the counters are
// constant thread-locals, which allocate nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's valid layout, unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            count(layout.size().cast_signed());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count(-layout.size().cast_signed());
        // SAFETY: the matching pointer and layout, unchanged.
        unsafe { System.dealloc(pointer, layout) };
    }
}

fn count(bytes: isize) {
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = LIVE.try_with(|live| live.set(live.get() + bytes));
        }
    });
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A creation that finds another holding the spawn lock waits, takes the lock and leaves its
/// thread's heap as it found it: the wait's own bookkeeping keeps nothing. A records write runs
/// its lock file's open through this path, and a test that counts what one write keeps
/// (aterm-secrets' `server_record_persistence_memory`) failed whenever another test's creation
/// held the lock. NEGATIVE CONTROL: with the exclusive waiters kept in a `Mutex<Vec<ThreadId>>`
/// again, as the seam kept them before this test, the waiter keeps that list's first buffer and,
/// when it locks the mutex first, Darwin's boxed mutex.
#[test]
fn a_creation_that_waits_for_the_spawn_lock_keeps_no_heap() {
    let (held_tx, held) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        aterm_uds::kept_out(|| {
            held_tx.send(()).expect("the test waits for the hold");
            let _ = released.recv_timeout(HANG);
            std::fs::File::open("/dev/null")
        })
    });
    held.recv_timeout(HANG)
        .expect("the holder holds the spawn lock");
    let waiter = std::thread::spawn(|| {
        COUNTING.with(|on| on.set(true));
        let made = aterm_uds::kept_out(|| std::fs::File::open("/dev/null"));
        COUNTING.with(|on| on.set(false));
        (made.map(drop), LIVE.with(Cell::get))
    });
    let started = Instant::now();
    while seam::exclusive_waiters() == 0 {
        assert!(
            !waiter.is_finished(),
            "the waiter took the spawn lock without waiting for it"
        );
        assert!(
            started.elapsed() < HANG,
            "the waiter never waited for the spawn lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).expect("the holder waits for its release");
    drop(
        holder
            .join()
            .expect("the holder")
            .expect("the holder's open"),
    );
    let (made, live) = waiter.join().expect("the waiter");
    made.expect("the waiter's open");
    assert_eq!(
        live, 0,
        "a wait for the spawn lock kept heap on the waiting thread"
    );
}
