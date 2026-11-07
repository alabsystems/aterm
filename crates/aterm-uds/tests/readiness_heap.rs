// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Count allocations on the calling thread, independently of the test runner.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::time::Instant;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn count() {
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = CALLS.try_with(|calls| calls.set(calls.get() + 1));
        }
    });
}

// SAFETY: every operation forwards the allocator's original pointer and layout
// to System. The constant thread-local counters allocate nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller supplied this valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        // SAFETY: the pointer, original layout and new size come from the caller.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the matching pointer and layout come from the caller.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn measured<T>(work: impl FnOnce() -> T) -> (T, usize) {
    CALLS.with(|calls| calls.set(0));
    COUNTING.with(|on| on.set(true));
    let value = work();
    COUNTING.with(|on| on.set(false));
    (value, CALLS.with(Cell::get))
}

#[test]
fn small_readiness_sets_allocate_only_the_ready_result() {
    use aterm_uds::ready::{self, Interest, Watch};

    let (mut reader, mut writer) = std::io::pipe().unwrap();
    for size in [1, 2, 8] {
        let fds = vec![reader.as_fd(); size];
        let interests = vec![Watch::new(reader.as_fd(), Interest::Read); size];
        let expired = Instant::now();
        let (result, calls) = measured(|| ready::any_until(&fds, expired).unwrap());
        assert!(result.is_empty());
        assert_eq!(calls, 0, "expired read set of {size}");
        let (result, calls) = measured(|| ready::any_interests_until(&interests, expired).unwrap());
        assert!(result.is_empty());
        assert_eq!(calls, 0, "expired interest set of {size}");
        let (result, calls) = measured(|| ready::any_interests_now(&interests).unwrap());
        assert!(result.is_empty());
        assert_eq!(calls, 0, "quiet interest set of {size}");
    }
    writer.write_all(b"x").unwrap();
    let interests = [Watch::new(reader.as_fd(), Interest::Read)];
    let (result, calls) = measured(|| ready::any_interests_now(&interests).unwrap());
    assert_eq!(result, [0]);
    assert_eq!(calls, 1, "only the returned index vector allocates");
    let mut byte = [0];
    reader.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
}
