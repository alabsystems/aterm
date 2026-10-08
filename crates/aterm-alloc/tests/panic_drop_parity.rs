// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A destructor that panics must not cause a double drop.
//!
//! The differential oracle's `Drop` impls are all infallible, so it cannot see
//! this. `clear`/`truncate` must lower `len` before dropping: otherwise a panic
//! mid-loop unwinds into `Drop for ArrayVec` (or `SmallVec`), which clears again
//! over the already-dropped prefix. Any type here that manages raw memory needs at least
//! one test whose destructor fails.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use aterm_alloc::{ArrayVec, SmallVec};

thread_local! {
    static DROPS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Records its index when dropped, and panics on the way out if it is `boom`.
struct Recorder {
    index: usize,
    boom: bool,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        DROPS.with(|d| d.borrow_mut().push(self.index));
        if self.boom {
            panic!("destructor for element {} panics on purpose", self.index);
        }
    }
}

fn drain_log() -> Vec<usize> {
    DROPS.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

fn filled(boom_at: usize) -> ArrayVec<Recorder, 8> {
    let mut v: ArrayVec<Recorder, 8> = ArrayVec::new();
    for index in 0..5 {
        let _ = v.try_push(Recorder {
            index,
            boom: index == boom_at,
        });
    }
    v
}

/// THE CONTROL. Without this, a `clear` that dropped NOTHING would satisfy the
/// no-double-drop assertion below, and this file would be vacuous.
#[test]
fn the_recorder_actually_records_and_actually_panics() {
    let _ = drain_log();
    {
        let mut v: ArrayVec<Recorder, 8> = ArrayVec::new();
        let _ = v.try_push(Recorder {
            index: 7,
            boom: false,
        });
        v.clear();
    }
    assert_eq!(
        drain_log(),
        vec![7],
        "an infallible Drop must be logged exactly once"
    );

    let caught = catch_unwind(AssertUnwindSafe(|| {
        let mut v: ArrayVec<Recorder, 8> = ArrayVec::new();
        let _ = v.try_push(Recorder {
            index: 9,
            boom: true,
        });
        v.clear();
    }));
    assert!(caught.is_err(), "the panicking Drop must actually unwind");
    let _ = drain_log();
}

#[test]
fn clear_does_not_double_drop_when_a_destructor_panics() {
    let _ = drain_log();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let mut v = filled(2);
        v.clear();
    }));
    assert!(
        caught.is_err(),
        "the panicking destructor must unwind out of clear"
    );

    let log = drain_log();
    let mut seen = log.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        log.len(),
        seen.len(),
        "every element must be dropped AT MOST once; got {log:?} — a repeated \
         index is a double free, which is what this test exists to catch"
    );
}

#[test]
fn truncate_does_not_double_drop_when_a_destructor_panics() {
    let _ = drain_log();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let mut v = filled(3);
        v.truncate(1);
    }));
    assert!(
        caught.is_err(),
        "the panicking destructor must unwind out of truncate"
    );

    let log = drain_log();
    let mut seen = log.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        log.len(),
        seen.len(),
        "every element must be dropped AT MOST once; got {log:?}"
    );
    // Element 0 is expected exactly once: `truncate` set `len = 1` before
    // dropping, so the retained prefix is still live when the panic unwinds
    // into `Drop for ArrayVec`, and is dropped there.
    assert!(
        log.contains(&0),
        "the retained element must still be dropped once, by the vector's own \
         Drop during unwinding; got {log:?}"
    );
}

#[test]
fn small_vec_inline_clear_does_not_double_drop_when_a_destructor_panics() {
    let _ = drain_log();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let mut v: SmallVec<Recorder, 8> = SmallVec::new();
        for index in 0..5 {
            v.push(Recorder {
                index,
                boom: index == 2,
            });
        }
        assert!(v.is_inline());
        v.clear();
    }));
    assert!(
        caught.is_err(),
        "the panicking destructor must unwind out of clear"
    );

    let log = drain_log();
    assert!(
        log.contains(&2),
        "the panicking element was dropped; got {log:?}"
    );
    let mut seen = log.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        log.len(),
        seen.len(),
        "every element must be dropped AT MOST once; got {log:?}"
    );
}
