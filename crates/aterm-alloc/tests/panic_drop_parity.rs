// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that panicking element destructors drop every element in [`ArrayVec`] and [`SmallVec`]
//! once.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use aterm_alloc::{ArrayVec, SmallVec};

thread_local! {
    static DROPS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

const LEN: usize = 5;

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

const fn recorder(index: usize, boom_at: usize) -> Recorder {
    Recorder {
        index,
        boom: index == boom_at,
    }
}

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

fn small<const N: usize>(boom_at: usize) -> SmallVec<Recorder, N> {
    let mut v: SmallVec<Recorder, N> = SmallVec::new();
    for index in 0..LEN {
        v.push(recorder(index, boom_at));
    }
    assert_eq!(v.is_inline(), N >= LEN);
    v
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
    let mut it = filled(3).into_iter();
    drop(it.next());
    let caught = catch_unwind(AssertUnwindSafe(|| drop(it)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_each_dropped_exactly_once("ArrayVec's IntoIter drop");
}

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
    let mut it = small::<N>(2).into_iter();
    drop(it.next());
    drop(it.next_back());
    let caught = catch_unwind(AssertUnwindSafe(|| drop(it)));
    assert!(caught.is_err(), "the panicking destructor must unwind");
    assert_each_dropped_exactly_once(&format!("SmallVec<_, {N}>'s IntoIter drop"));
}

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

fn ids_disarmed(held: &mut [Recorder]) -> Vec<usize> {
    held.iter_mut()
        .map(|r| {
            r.boom = false;
            r.index
        })
        .collect()
}

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
