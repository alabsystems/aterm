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
//! aterm-agent's accounts store open, aterm-cli's doctor lock open and record write, the table and
//! control Labs of aterm-db's tests), waiting there at most [`LET_IN_AFTER`], and keeps it until
//! the test ends or walks. aterm-db's tests walk a file system world per item on [`each`], whose
//! workers each take a place the same way while the caller gives its own up, so such a walk uses
//! the places the rest of the binary leaves free. The other threads a test starts, the workers of
//! a plain `check::each_on(PLACES, ..)` among them, never wait here, so they keep their pace with
//! the test's thread and with each other. A test that reaches no such call is not counted, and nor
//! are those threads.
//!
//! The lane is a process's (one test binary's, across every crate it links): next to another
//! file system heavy process it does not pace, the binary gives way to it and finishes later
//! than it would have without the lane.

use std::cell::RefCell;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How many of libtest's threads past [`join`] and workers of [`each`] run at once, short of the
/// ones let in over it ([`LET_IN_AFTER`]): the count past which aterm-db's lib tests, the longest
/// of these binaries' tests, buy little wall time for much more kernel time. One whole-binary run
/// of each at a 60 s let-in, measured 2026-10-31: no lane 250 s (2086 s in the kernel), three
/// places 186 s (359 s), four 162 s (480 s), five 158 s (668 s), six 165 s (909 s).
pub const PLACES: usize = 4;

/// The longest a thread waits for a place before it goes in over [`PLACES`]: the lane paces and
/// never fails a test, and holds a thread no longer than this at each wait, since a test holding a
/// place may wait on one that wants one (a process-wide lock or lease between them) or simply run
/// long. It outlasts most of the tests it waits for, or they would all be let in and the lane would
/// pace little: with a walk's workers not yet holding places, aterm-db's lib tests took 201 s (1089
/// s in the kernel) at 5 s and 175 s (395 s) at 120 s, three places; with them, 162 s (480 s) at
/// 60 s and 160 s (472 s) at 30 s, four places. A test whose first call here comes inside one of
/// its own deadlines keeps what this wait leaves of it.
#[cfg(not(test))]
pub const LET_IN_AFTER: Duration = Duration::from_secs(30);

/// [`LET_IN_AFTER`] under this crate's own tests, whose holders always give their places back: the
/// one-minute hang detector ([`crate::wait::MUST_HAPPEN`]), so a stalled holder is not let in over,
/// which the tests that count the places would read as the lane failing.
#[cfg(test)]
pub const LET_IN_AFTER: Duration = crate::wait::MUST_HAPPEN;

/// The places taken now.
static TAKEN: Mutex<usize> = Mutex::new(0);

/// Signalled as a place is given back.
static GIVEN_BACK: Condvar = Condvar::new();

/// One place, given back when the thread that took it ends.
struct Place;

impl Drop for Place {
    fn drop(&mut self) {
        let mut taken = TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
        *taken = taken.saturating_sub(1);
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
    if std::thread::current()
        .name()
        .is_some_and(|name| name.contains("::"))
    {
        take(&|| false);
    }
}

/// `work` over every item, results in item order, each worker holding a place of its own: a walk
/// that builds a file system world per item uses every place the rest of the binary leaves free,
/// and no more. The calling thread gives its place up for the walk; a test's thread takes one
/// again at its next [`join`], and a worker of a walk around this one goes on without one.
pub fn each<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let _ = HELD.try_with(|held| held.borrow_mut().take());
    crate::check::each_after(PLACES, |spent| take(spent), items, work)
}

/// Remove `dir` when this process exits normally: for a fixture a `static` keeps for the whole
/// run, whose `Drop` never runs. libtest's exit, failed tests included, is one; a kill leaves it.
#[cfg_attr(trust_verify, trust::skip)]
pub fn remove_at_exit(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        static DIRS: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());
        static ARMED: std::sync::Once = std::sync::Once::new();
        extern "C" fn at_exit() {
            for dir in DIRS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain(..)
            {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        unsafe extern "C" {
            fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
        }
        DIRS.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(dir.to_path_buf());
        ARMED.call_once(|| {
            // SAFETY: registers a plain `extern "C"` function with no captured state; a failed registration leaves `dir` for the next temporary-folder sweep.
            unsafe { atexit(at_exit) };
        });
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// How often a thread waiting for a place reads whether it still wants one.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Give this thread a place unless it holds one, waiting at most [`LET_IN_AFTER`], or until `spent`
/// says it has no use for one: a walk's worker leaves the queue once the other workers have claimed
/// every item, with no place, rather than hold its finished walk open.
fn take(spent: &dyn Fn() -> bool) {
    let _ = HELD.try_with(|held| {
        if held.borrow().is_some() {
            return;
        }
        let until = Instant::now().checked_add(LET_IN_AFTER);
        let mut taken = TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
        while *taken >= PLACES {
            if spent() {
                return;
            }
            let left = until.map_or(LET_IN_AFTER, |u| {
                u.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                break;
            }
            taken = GIVEN_BACK
                .wait_timeout(taken, left.min(LOOK_AGAIN))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *taken = taken.saturating_add(1);
        drop(taken);
        *held.borrow_mut() = Some(Place);
    });
}

#[cfg(test)]
#[path = "fs_lane_tests.rs"]
mod tests;
