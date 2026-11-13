// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Count allocations on the calling thread, independently of the test runner.
#![cfg(unix)]

use aterm_bench::allocator::{Observed, Observer};
use std::alloc::System;
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
            let _ = CALLS.try_with(|calls| calls.set(calls.get().wrapping_add(1)));
        }
    });
}

// SAFETY: constant, drop-free thread-locals and wrapping arithmetic neither allocate nor unwind.
unsafe impl Observer for Counting {
    fn requested(&self, _size: usize) {
        count();
    }
}

#[global_allocator]
static ALLOCATOR: Observed<System, Counting> = Observed::new(System, Counting);

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

    let (mut reader, mut writer) = aterm_uds::pipe().unwrap();
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
