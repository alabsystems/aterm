// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A DESTRUCTOR THAT PANICS MUST NOT CAUSE A DOUBLE DROP.
//!
//! This file exists because the differential oracle beside it could not see the
//! one class of behaviour where the two implementations actually differed. It
//! drives both through `clear` and `truncate` and counts drops with `Bomb` and
//! `Tracked` — but every `Drop` impl in it is INFALLIBLE, and the divergence
//! only appears when a destructor unwinds.
//!
//! The bug it missed: `clear`/`truncate` dropped their elements and only then
//! updated `len`. A panic partway through the loop unwound into
//! `Drop for ArrayVec`, which calls `clear` again over a `len` that still
//! described the ALREADY-DROPPED prefix, and dropped it a second time. Measured
//! before the fix, the drop log read `[0, 1, 0, 1, 2]` for `clear` and
//! `[2, 0, 1, 2, 3]` for `truncate`; upstream logs each index exactly once.
//!
//! The rule this encodes, for the next data structure written here: an oracle
//! that only ever exercises the happy path proves the happy path. If a type
//! manages raw memory, at least one test must make a destructor FAIL.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use aterm_alloc::ArrayVec;

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
    // Element 0 IS expected here, exactly once: `truncate` set `len = 1` before
    // it dropped anything, so when the panic unwinds into `Drop for ArrayVec`
    // the retained prefix is still live and is dropped then. That is the fix
    // working, not a leak — the assertion that matters is the no-duplicates one
    // above. (This comment exists because the first version of this test
    // asserted `!log.contains(&0)` and failed on correct behaviour.)
    assert!(
        log.contains(&0),
        "the retained element must still be dropped once, by the vector's own \
         Drop during unwinding; got {log:?}"
    );
}
