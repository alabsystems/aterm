// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How many of a test binary's file system heavy tests run at once (test support: each caller
//! makes its call `#[cfg(test)]`, and no shipped crate links aterm-spec, which only the model
//! crates take as a normal dependency).
//!
//! macOS serves file system calls (open, unlink, mkdir, rename) no faster past about four
//! callers at once, machine-wide: at fourteen each call costs ten to twenty-five times what it
//! costs alone (measured 2026-10-28: open, read and close 9 µs alone and 152 µs at fourteen;
//! rename 106 µs and 2.7 ms). A test binary whose tests do little else spends most of its time
//! spinning in the kernel on all of libtest's threads (aterm-agent's lib tests 550-930 s of sys
//! time, aterm-cli's 166-189 s in 17-18 s of wall time). So such a test's own thread takes one of
//! [`PLACES`] where its file system work starts ([`join`], from its crate's `#[cfg(test)]` code:
//! aterm-agent's accounts store open, aterm-cli's doctor lock open and record write), waiting
//! there at most [`LET_IN_AFTER`], and keeps it until the test ends. The threads a test starts
//! never wait here, so past that one wait of the test's thread they keep their pace with it and
//! with each other. A test that reaches no such call is not counted, and nor are the threads a
//! test starts.
//!
//! The lane is a process's (one test binary's, across every crate it links): next to another
//! file system heavy process it does not pace, the binary gives way to it and finishes later
//! than it would have without the lane.

use std::cell::RefCell;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How many tests that have reached [`join`] run at once, short of the ones let in over it
/// ([`LET_IN_AFTER`]): the fewest at which aterm-agent's lib tests run fastest. Its whole-binary
/// runs measured 2026-10-28, two each, took 64.9 and 63.7 s (143 and 144 s in the kernel)
/// with three places, 65.8 and 63.3 s (207 and 199 s) with four, 72.5 and 70.2 s (362 and
/// 352 s) with six, and 100.2 and 103.1 s (824 and 857 s) with no lane.
pub const PLACES: usize = 3;

/// The longest a test waits for a place before it goes in over [`PLACES`]: the lane paces and
/// never fails or holds a test for long, since a test holding a place may wait on one that
/// wants one (a process-wide lock or lease between them) or simply run long, and a test whose
/// first call here comes inside one of its own deadlines keeps most of it.
pub const LET_IN_AFTER: Duration = Duration::from_secs(5);

/// The places taken now.
static TAKEN: Mutex<usize> = Mutex::new(0);

/// Signalled as a place is given back.
static GIVEN_BACK: Condvar = Condvar::new();

/// One place, given back when the thread that took it ends.
struct Place;

impl Drop for Place {
    fn drop(&mut self) {
        *TAKEN.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        GIVEN_BACK.notify_one();
    }
}

thread_local! {
    static HELD: RefCell<Option<Place>> = const { RefCell::new(None) };
}

/// Take this test's place as its file system work starts, once per test. Only libtest's own
/// test threads, which it names after their tests (`module::test`), take one; the threads a test
/// starts pass at once, and so does a call made after the thread's place has gone at its end.
pub fn join() {
    if !std::thread::current()
        .name()
        .is_some_and(|name| name.contains("::"))
    {
        return;
    }
    let _ = HELD.try_with(|held| {
        if held.borrow().is_some() {
            return;
        }
        let until = Instant::now() + LET_IN_AFTER;
        let mut taken = TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
        while *taken >= PLACES {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            taken = GIVEN_BACK
                .wait_timeout(taken, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *taken += 1;
        drop(taken);
        *held.borrow_mut() = Some(Place);
    });
}
