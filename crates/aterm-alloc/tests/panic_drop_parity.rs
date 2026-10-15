// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A destructor that panics must drop every element EXACTLY once: no double
//! drop, and no leak of the elements after it.
//!
//! The differential oracle's `Drop` impls are all infallible, so it cannot see
//! this. Every bulk drop here — `clear`, `truncate`, the vectors' own `Drop`,
//! a by-value iterator's `Drop` and `Drain`'s — lowers the length first, so a
//! panic that unwinds into `Drop for ArrayVec` (or `SmallVec`) cannot drop the
//! prefix again, and drops the cut slots with slice drop glue, which keeps
//! dropping past a panicking element, as std `Vec` and upstream `arrayvec` do.
//! `ArrayVec::retain` drops the elements it rejects one at a time, so a
//! panicking one must already be outside the range its guard keeps.
//! An "at most once" assertion let a leak of every later element through, so
//! each test here checks the whole drop log against `0..LEN`. Any type here
//! that manages raw memory needs at least one test whose destructor fails.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use aterm_alloc::{ArrayVec, SmallVec};

thread_local! {
    static DROPS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Elements per vector; the panicking one is somewhere in the middle.
const LEN: usize = 5;

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

fn recorder(index: usize, boom_at: usize) -> Recorder {
    Recorder {
        index,
        boom: index == boom_at,
    }
}

/// Every one of the `LEN` elements was dropped, and none twice. A repeated
/// index is a double free; a missing one is a leak.
fn assert_each_dropped_exactly_once(what: &str) {
    assert_exactly_once(&drain_log(), what);
}

fn assert_exactly_once(log: &[usize], what: &str) {
    let mut sorted = log.to_vec();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        (0..LEN).collect::<Vec<_>>(),
        "{what}: every element must be dropped EXACTLY once; drop log {log:?}"
    );
}

fn ids<'a>(v: impl IntoIterator<Item = &'a Recorder>) -> Vec<usize> {
    v.into_iter().map(|r| r.index).collect()
}

fn filled(boom_at: usize) -> ArrayVec<Recorder, 8> {
    let mut v: ArrayVec<Recorder, 8> = ArrayVec::new();
    for index in 0..LEN {
        let _ = v.try_push(recorder(index, boom_at));
    }
    v
}

/// A `SmallVec` of `LEN` elements: inline for `N >= LEN`, spilled otherwise.
/// The spilled arm delegates to std `Vec`, so it is the inline arm's oracle.
fn small<const N: usize>(boom_at: usize) -> SmallVec<Recorder, N> {
    let mut v: SmallVec<Recorder, N> = SmallVec::new();
    for index in 0..LEN {
        v.push(recorder(index, boom_at));
    }
    assert_eq!(v.is_inline(), N >= LEN);
    v
}

/// THE CONTROL. Without this, a `clear` that dropped NOTHING (or a destructor
/// that never panicked) would satisfy the assertions below vacuously.
#[test]
fn the_recorder_actually_records_and_actually_panics() {
    let _ = drain_log();
    {
        let mut v: ArrayVec<Recorder, 8> = ArrayVec::new();
        let _ = v.try_push(recorder(7, usize::MAX));
        v.clear();
    }
    assert_eq!(
        drain_log(),
        vec![7],
        "an infallible Drop must be logged exactly once"
    );

    let caught = catch_unwind(AssertUnwindSafe(|| {
        let mut v: ArrayVec<Recorder, 8> = ArrayVec::new();
        let _ = v.try_push(recorder(9, 9));
        v.clear();
    }));
    assert!(caught.is_err(), "the panicking Drop must actually unwind");
    let _ = drain_log();
}

#[test]
fn array_vec_clear_drops_each_element_once_when_a_destructor_panics() {
    let _ = drain_log();
    let mut v = filled(2);
    let caught = catch_unwind(AssertUnwindSafe(|| v.clear()));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert!(v.is_empty(), "clear empties the vec even when it unwinds");
    drop(v);
    assert_each_dropped_exactly_once("ArrayVec::clear");
}

#[test]
fn array_vec_truncate_drops_each_element_once_when_a_destructor_panics() {
    let _ = drain_log();
    let mut v = filled(3);
    let caught = catch_unwind(AssertUnwindSafe(|| v.truncate(1)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_eq!(ids(&v), vec![0], "the retained prefix survives the unwind");
    drop(v);
    assert_each_dropped_exactly_once("ArrayVec::truncate");
}

#[test]
fn array_vec_drop_drops_each_element_once_when_a_destructor_panics() {
    let _ = drain_log();
    let v = filled(2);
    let caught = catch_unwind(AssertUnwindSafe(|| drop(v)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_each_dropped_exactly_once("Drop for ArrayVec");
}

#[test]
fn array_vec_into_iter_drop_drops_each_element_once_when_a_destructor_panics() {
    let _ = drain_log();
    // 0 is yielded; 1, 2, 3 (which panics) and 4 are left to `Drop`.
    let mut it = filled(3).into_iter();
    drop(it.next());
    let caught = catch_unwind(AssertUnwindSafe(|| drop(it)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_each_dropped_exactly_once("ArrayVec's IntoIter drop");
}

/// The drained range is gone and the tail closed up even when one of the
/// range's destructors panics: std `Vec::Drain`'s outcome. (Upstream
/// `arrayvec` leaks the rest of the range and the tail there; leaving them in
/// the vec, as this did before, hands the caller elements it asked removed.)
#[test]
fn array_vec_drain_drop_removes_and_drops_the_range_once_when_a_destructor_panics() {
    let _ = drain_log();
    let mut v = filled(2);
    let caught = catch_unwind(AssertUnwindSafe(|| drop(v.drain(1..4))));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_eq!(
        ids(&v),
        vec![0, 4],
        "the drained range is removed and the tail closed up"
    );
    drop(v);
    assert_each_dropped_exactly_once("ArrayVec::drain(1..4) dropped");

    // Partly consumed first: the yielded element is the caller's, the rest of
    // the range still goes.
    let mut v = filled(3);
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let mut d = v.drain(1..);
        drop(d.next());
        drop(d);
    }));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_eq!(ids(&v), vec![0], "the whole drained range is removed");
    drop(v);
    assert_each_dropped_exactly_once("ArrayVec::drain(1..) partly consumed");
}

fn small_vec_clear_case<const N: usize>() {
    let _ = drain_log();
    let mut v = small::<N>(2);
    let caught = catch_unwind(AssertUnwindSafe(|| v.clear()));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert!(v.is_empty(), "clear empties the vec even when it unwinds");
    drop(v);
    assert_each_dropped_exactly_once(&format!("SmallVec<_, {N}>::clear"));
}

fn small_vec_drop_case<const N: usize>() {
    let _ = drain_log();
    let v = small::<N>(2);
    let caught = catch_unwind(AssertUnwindSafe(|| drop(v)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_each_dropped_exactly_once(&format!("Drop for SmallVec<_, {N}>"));
}

fn small_vec_into_iter_case<const N: usize>() {
    let _ = drain_log();
    // 0 and 4 are yielded; 1, 2 (which panics) and 3 are left to `Drop`.
    let mut it = small::<N>(2).into_iter();
    drop(it.next());
    drop(it.next_back());
    let caught = catch_unwind(AssertUnwindSafe(|| drop(it)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_each_dropped_exactly_once(&format!("SmallVec<_, {N}>'s IntoIter drop"));
}

/// Inline (`N = 8`) and spilled (`N = 2`, std `Vec` underneath) must agree:
/// whether a `SmallVec` spilled is not part of its drop contract.
#[test]
fn small_vec_clear_drops_each_element_once_when_a_destructor_panics() {
    small_vec_clear_case::<8>();
    small_vec_clear_case::<2>();
}

#[test]
fn small_vec_drop_drops_each_element_once_when_a_destructor_panics() {
    small_vec_drop_case::<8>();
    small_vec_drop_case::<2>();
}

#[test]
fn small_vec_into_iter_drop_drops_each_element_once_when_a_destructor_panics() {
    small_vec_into_iter_case::<8>();
    small_vec_into_iter_case::<2>();
}

/// The ids `held` holds, in order, with every element disarmed: an element a
/// broken `retain` kept after dropping it then cannot panic a second time and
/// abort the run before the assertions report it.
fn ids_disarmed(held: &mut [Recorder]) -> Vec<usize> {
    held.iter_mut()
        .map(|r| {
            r.boom = false;
            r.index
        })
        .collect()
}

/// `retain` whose REJECT arm's element destructor panics, at every position
/// and under every keep mask that rejects it, against std `Vec::retain_mut`
/// over the same elements: whether it unwinds, what the vec holds afterwards
/// (the kept ones plus every element after the panic), the drops during the
/// unwind and the drops afterwards, then exactly once overall. `retain` must
/// move past an element before dropping it: otherwise its guard keeps the
/// dropped element and it is dropped twice.
#[test]
fn array_vec_retain_drops_each_element_once_when_a_rejected_destructor_panics() {
    for boom_at in 0..LEN {
        for keep_mask in 0..(1usize << LEN) {
            if keep_mask & (1 << boom_at) != 0 {
                continue; // a kept element's destructor never runs in `retain`
            }
            let keep = |r: &mut Recorder| keep_mask & (1 << r.index) != 0;
            let what = format!("retain, destructor {boom_at} panics, keep mask {keep_mask:#07b}");
            let _ = drain_log();

            let mut model: Vec<Recorder> = (0..LEN).map(|i| recorder(i, boom_at)).collect();
            let model_unwound = catch_unwind(AssertUnwindSafe(|| model.retain_mut(keep))).is_err();
            let model_during = drain_log();
            let model_held = ids_disarmed(&mut model);
            drop(model);
            let model_after = drain_log();

            let mut mine = filled(boom_at);
            let mine_unwound = catch_unwind(AssertUnwindSafe(|| mine.retain(keep))).is_err();
            let mine_during = drain_log();
            let mine_held = ids_disarmed(mine.as_mut_slice());
            drop(mine);
            let mine_after = drain_log();

            assert!(model_unwound, "{what}: the model must unwind");
            assert_eq!(mine_unwound, model_unwound, "{what}: unwinds");
            assert_eq!(mine_held, model_held, "{what}: contents after the unwind");
            assert_eq!(mine_during, model_during, "{what}: drops during the unwind");
            assert_eq!(mine_after, model_after, "{what}: drops afterwards");
            let all: Vec<usize> = mine_during.iter().chain(&mine_after).copied().collect();
            assert_exactly_once(&all, &what);
        }
    }
}
