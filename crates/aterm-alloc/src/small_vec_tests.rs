// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn ordered_edits_match_vec<const N: usize>() {
    for len in 0..8 {
        for at in 0..=len {
            let mut actual = SmallVec::<usize, N>::new();
            let mut expected: Vec<_> = (0..len).collect();
            for value in &expected {
                actual.push(*value);
            }
            actual.insert(at, 99);
            expected.insert(at, 99);
            assert_eq!(actual.as_slice(), expected);
            assert_eq!(actual.remove(at), expected.remove(at));
            assert_eq!(actual.as_slice(), expected);
            for keep in (0..=len + 1).rev() {
                actual.truncate(keep);
                expected.truncate(keep);
                assert_eq!(actual.as_slice(), expected);
            }
        }
    }
}

#[test]
fn ordered_edits_match_vec_before_and_after_spilling() {
    ordered_edits_match_vec::<0>();
    ordered_edits_match_vec::<1>();
    ordered_edits_match_vec::<4>();
}
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

struct DropCounter {
    counter: std::rc::Rc<std::cell::Cell<usize>>,
    payload: String,
}

impl DropCounter {
    fn new(counter: &std::rc::Rc<std::cell::Cell<usize>>) -> Self {
        Self {
            counter: std::rc::Rc::clone(counter),
            payload: "into-iter drop payload".to_string(),
        }
    }
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        assert_eq!(self.payload, "into-iter drop payload");
        self.counter.set(self.counter.get() + 1);
    }
}

#[test]
fn test_into_iter_inline_double_ended() {
    let mut sv: SmallVec<i32, 4> = SmallVec::new();
    sv.push(1);
    sv.push(2);
    sv.push(3);
    sv.push(4);
    assert!(sv.is_inline());
    let mut it = sv.into_iter();
    assert_eq!(it.next(), Some(1));
    assert_eq!(it.next_back(), Some(4));
    assert_eq!(it.len(), 2);
    assert_eq!(it.next(), Some(2));
    assert_eq!(it.next_back(), Some(3));
    assert_eq!(it.next(), None);
    assert_eq!(it.next_back(), None);
    assert_eq!(
        heap_spill_capacity::<()>((usize::MAX / 2) + 1, usize::MAX),
        0
    );
    assert_eq!(heap_spill_capacity::<i32>(4, 4), 8);
    assert_eq!(
        heap_spill_capacity::<u8>((isize::MAX as usize / 2) + 1, (isize::MAX as usize / 2) + 1),
        isize::MAX as usize
    );
}

fn drops_each_once<const N: usize>(len: usize, front: usize, back: usize) {
    let case = format!("N={N} len={len} front={front} back={back}");
    let counter = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut sv: SmallVec<DropCounter, N> = SmallVec::new();
    for _ in 0..len {
        sv.push(DropCounter::new(&counter));
    }
    assert_eq!(sv.is_inline(), len <= N, "{case}");
    let mut it = sv.into_iter();
    let mut taken = Vec::new();
    for _ in 0..front {
        taken.push(it.next().expect("a front element"));
    }
    for _ in 0..back {
        taken.push(it.next_back().expect("a back element"));
    }
    assert_eq!(counter.get(), 0, "{case}: yielded elements are alive");
    drop(it);
    assert_eq!(
        counter.get(),
        len - taken.len(),
        "{case}: the iterator drops what it still holds"
    );
    drop(taken);
    assert_eq!(counter.get(), len, "{case}: each element dropped once");
}

#[test]
fn into_iter_drops_each_element_exactly_once() {
    for (front, back) in [(0, 0), (1, 0), (1, 1), (3, 0)] {
        drops_each_once::<4>(3, front, back);
    }
    for (front, back) in [(0, 0), (2, 0), (1, 1), (5, 0)] {
        drops_each_once::<2>(5, front, back);
    }
}
