// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn an_empty_allocation_is_its_alignment() {
    assert_eq!(alloc(0, 1), Some(1));
    assert_eq!(alloc(0, 8), Some(8));
    assert_eq!(alloc(4, 3), None, "not a power of two");
    // SAFETY: a zero-size "allocation" frees nothing.
    unsafe { free(8, 0, 8) };
}

#[test]
fn realloc_keeps_the_prefix_in_both_directions() {
    let p = alloc(4, 1).unwrap();
    // SAFETY: `p` is a live 4-byte allocation; each step below uses the pointer and size the
    // previous one returned.
    unsafe {
        std::ptr::copy_nonoverlapping(b"abcd".as_ptr(), p as *mut u8, 4);
        let grown = realloc(p, 4, 64, 1).unwrap();
        assert_eq!(std::slice::from_raw_parts(grown as *const u8, 4), b"abcd");
        let shrunk = realloc(grown, 64, 2, 1).unwrap();
        assert_eq!(std::slice::from_raw_parts(shrunk as *const u8, 2), b"ab");
        assert_eq!(realloc(shrunk, 2, 0, 1), Some(1));
        let fresh = realloc(1, 0, 3, 1).unwrap();
        free(fresh, 3, 1);
    }
}

#[test]
fn the_slot_is_per_thread_and_holds_the_last_value() {
    set_ret(7);
    assert_eq!(ret(), 7);
    // SAFETY: the slot starts with a live `u32` for this thread's lifetime.
    assert_eq!(unsafe { *(ret_slot() as *const u32) }, 7);
    let other = std::thread::spawn(|| (ret(), ret_slot())).join().unwrap();
    assert_eq!(other.0, 0);
    assert_ne!(other.1, ret_slot());
}

#[test]
fn a_throw_waits_in_the_slot_until_taken() {
    assert_eq!(take_thrown(), None);
    set_thrown(Thrown::Value("bad blob".into()));
    // SAFETY: the slot is live for this thread; `thrown` is its second `u32` (repr(C)).
    assert_eq!(unsafe { *((ret_slot() + 4) as *const u32) }, thrown::VALUE);
    assert_eq!(take_thrown(), Some(Thrown::Value("bad blob".into())));
    assert_eq!(take_thrown(), None, "taken once");
    set_thrown(Thrown::Error(String::new()));
    assert_eq!(take_thrown(), Some(Thrown::Error(String::new())));
    set_thrown(Thrown::Error("first".into()));
    set_thrown(Thrown::Value("second".into()));
    assert_eq!(take_thrown(), Some(Thrown::Value("second".into())));
}
