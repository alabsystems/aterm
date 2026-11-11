// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::format;

#[test]
fn test_into_inner_moves_without_double_drop() {
    let counter = std::rc::Rc::new(std::cell::Cell::new(0));
    struct Bomb(std::rc::Rc<std::cell::Cell<usize>>);
    impl Drop for Bomb {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let mut av: ArrayVec<Bomb, 2> = ArrayVec::new();
    av.push(Bomb(std::rc::Rc::clone(&counter)));
    av.push(Bomb(std::rc::Rc::clone(&counter)));
    let arr = av.into_inner().unwrap_or_else(|_| unreachable!());
    assert_eq!(counter.get(), 0, "into_inner must not drop");
    drop(arr);
    assert_eq!(counter.get(), 2, "each element dropped exactly once");
}

#[test]
fn test_drain_double_ended() {
    let mut av: ArrayVec<i32, 8> = ArrayVec::new();
    av.extend([1, 2, 3, 4, 5]);
    {
        let mut d = av.drain(1..4);
        assert_eq!(d.next_back(), Some(4));
        assert_eq!(d.next(), Some(2));
        assert_eq!(d.next(), Some(3));
        assert_eq!(d.next(), None);
    }
    assert_eq!(av.as_slice(), &[1, 5]);
}

#[test]
fn test_drain_forget_keeps_unyielded_elements() {
    let counter = std::rc::Rc::new(std::cell::Cell::new(0));
    struct Tracked(i32, std::rc::Rc<std::cell::Cell<usize>>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.1.set(self.1.get() + 1);
        }
    }
    let mut av: ArrayVec<Tracked, 8> = ArrayVec::new();
    for v in [10, 20, 30, 40, 50] {
        av.push(Tracked(v, std::rc::Rc::clone(&counter)));
    }
    {
        let mut d = av.drain(1..4);
        assert_eq!(d.next().map(|t| t.0), Some(20));
        assert_eq!(d.next_back().map(|t| t.0), Some(40));
        assert_eq!(counter.get(), 2, "yielded elements dropped by caller");
        core::mem::forget(d);
    }
    assert_eq!(counter.get(), 2, "forgetting Drain drops nothing else");
    let remaining: [i32; 3] = [av[0].0, av[1].0, av[2].0];
    assert_eq!(av.len(), 3);
    assert_eq!(remaining, [10, 30, 50]);
    drop(av);
    assert_eq!(counter.get(), 5, "every element dropped once overall");
}

#[test]
#[should_panic(expected = "ArrayVec::drain: end")]
fn test_drain_past_len_panics() {
    let mut av: ArrayVec<i32, 8> = ArrayVec::new();
    av.extend([1, 2]);
    let _ = av.drain(0..3);
}

#[test]
fn test_capacity_error_shape() {
    let e = CapacityError::new(7u8);
    assert_eq!(format!("{e}"), "insufficient capacity");
    assert_eq!(format!("{e:?}"), "CapacityError: insufficient capacity");
    assert_eq!(e.simplify(), CapacityError::new(()));
    assert_eq!(e.element(), 7);
}
