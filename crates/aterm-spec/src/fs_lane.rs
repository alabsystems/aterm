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
pub const LET_IN_AFTER: Duration = Duration::from_secs(30);

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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// The tests that fill the lane, one at a time: two of them each short of a place they are
    /// waiting to see taken would wait for each other until the let-in.
    static FILLING: Mutex<()> = Mutex::new(());

    /// `then` with `holders` threads holding a place each.
    fn with_places_held(holders: usize, then: impl FnOnce()) {
        let _alone = FILLING.lock().unwrap_or_else(PoisonError::into_inner);
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        std::thread::scope(|scope| {
            let (took, taken) = mpsc::channel();
            for n in 0..holders {
                let (took, released) = (took.clone(), &released);
                std::thread::Builder::new()
                    .name(format!("holder::{n}"))
                    .spawn_scoped(scope, move || {
                        join();
                        took.send(()).unwrap();
                        let _ = released.lock().unwrap().recv();
                    })
                    .unwrap();
            }
            for _ in 0..holders {
                taken.recv().unwrap();
            }
            then();
            drop(release);
        });
    }

    /// Two walks at once share the places, and each still answers in item order.
    #[test]
    fn walks_at_once_share_the_places_and_answer_in_item_order() {
        let items: Vec<usize> = (0..48).collect();
        let (running, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let walk = || {
            each(&items, |&item| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
                running.fetch_sub(1, Ordering::SeqCst);
                item
            })
        };
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(walk);
            let second = scope.spawn(walk);
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(first, items);
        assert_eq!(second, items);
        assert!(most.load(Ordering::SeqCst) <= PLACES, "{most:?}");
    }

    /// A walk's caller gives its place up, so holding the last one cannot make its walk wait.
    #[test]
    fn a_walk_gives_its_callers_place_to_its_workers() {
        with_places_held(PLACES - 1, || {
            join();
            let walked = each(&[1, 2, 3], |&item| {
                let taken = *TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
                assert!(taken <= PLACES, "a worker went in over the places: {taken}");
                item
            });
            assert_eq!(walked, [1, 2, 3]);
        });
    }

    /// A thread with no use for a place leaves a full lane's queue holding none.
    #[test]
    fn a_thread_with_no_use_for_a_place_leaves_the_queue() {
        with_places_held(PLACES, || {
            let spent = AtomicBool::new(false);
            std::thread::scope(|scope| {
                let waiting = scope.spawn(|| {
                    take(&|| spent.load(Ordering::SeqCst));
                    HELD.with(|held| held.borrow().is_some())
                });
                spent.store(true, Ordering::SeqCst);
                assert!(
                    !waiting.join().unwrap(),
                    "it took a place it had no use for"
                );
            });
        });
    }
}
