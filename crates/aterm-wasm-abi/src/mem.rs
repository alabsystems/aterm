// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Memory the glue and the module hand each other.

use std::alloc::Layout;
use std::cell::Cell;

use crate::marshal::{IntoAbi, Thrown};

/// A new allocation of `size` bytes at `align`, or `None` for an invalid layout or an exhausted
/// heap.
#[must_use]
pub fn alloc(size: usize, align: usize) -> Option<usize> {
    let layout = Layout::from_size_align(size, align).ok()?;
    if size == 0 {
        return Some(align);
    }
    // SAFETY: the layout's size is non-zero (checked above).
    let ptr = unsafe { std::alloc::alloc(layout) };
    (!ptr.is_null()).then_some(ptr as usize)
}

/// Grow or shrink the allocation at `ptr` from `old_size` to `new_size` bytes, keeping its first
/// `min(old, new)` bytes.
/// # Safety
/// `ptr` is what [`alloc`] or this function returned for `old_size` bytes at `align`, and has not
/// been freed.
#[must_use]
pub unsafe fn realloc(ptr: usize, old_size: usize, new_size: usize, align: usize) -> Option<usize> {
    let old = Layout::from_size_align(old_size, align).ok()?;
    if old_size == 0 {
        return alloc(new_size, align);
    }
    if new_size == 0 {
        // SAFETY: the caller's contract: a live allocation of `old`.
        unsafe { free(ptr, old_size, align) };
        return Some(align);
    }
    Layout::from_size_align(new_size, align).ok()?;
    // SAFETY: the caller's contract, and `new_size` is non-zero and forms a valid layout at `align`
    // (checked above).
    let out = unsafe { std::alloc::realloc(ptr as *mut u8, old, new_size) };
    (!out.is_null()).then_some(out as usize)
}

/// Free `size` bytes at `align` at `ptr`.
/// # Safety
/// `ptr` is what [`alloc`] or [`realloc`] returned for exactly `size` bytes at `align` (or a buffer
/// the module handed out with that layout), and has not been freed.
pub unsafe fn free(ptr: usize, size: usize, align: usize) {
    if size == 0 {
        return;
    }
    // SAFETY: the caller's contract: this is the allocation's own layout, which was valid when it
    // was made.
    unsafe {
        std::alloc::dealloc(
            ptr as *mut u8,
            Layout::from_size_align_unchecked(size, align),
        );
    }
}

/// What [`Slot::thrown`] holds.
pub mod thrown {
    /// The call returned.
    pub const NONE: u32 = 0;
    /// `throw new Error(msg)` ([`crate::marshal::Thrown::Error`]).
    pub const ERROR: u32 = 1;
    /// `throw msg` ([`crate::marshal::Thrown::Value`]).
    pub const VALUE: u32 = 2;
}

/// The return slot. `repr(C)` of 32-bit words on wasm32: the glue reads the
/// value at byte 0, the throw at 4, and the message's pointer and length at
/// 8 and 12.
#[repr(C)]
struct Slot {
    /// A length, or whether an `Option` is `Some`.
    value: Cell<u32>,
    /// [`thrown`]: whether the call threw, and how.
    thrown: Cell<u32>,
    /// The thrown message: a UTF-8 buffer of `msg_len` bytes, the glue's to
    /// free once it has read it.
    msg_ptr: Cell<usize>,
    msg_len: Cell<usize>,
}

thread_local! {
    static RET: Slot = const {
        Slot {
            value: Cell::new(0),
            thrown: Cell::new(thrown::NONE),
            msg_ptr: Cell::new(0),
            msg_len: Cell::new(0),
        }
    };
}

/// The return slot's address, which the glue reads right after each export returns.
#[must_use]
pub fn ret_slot() -> usize {
    RET.with(|s| std::ptr::from_ref(s) as usize)
}

/// Leave `value` in the return slot for the glue.
pub fn set_ret(value: u32) {
    RET.with(|s| s.value.set(value));
}

/// What the return slot holds: what the last export left there.
#[must_use]
pub fn ret() -> u32 {
    RET.with(|s| s.value.get())
}

/// Leave `thrown` in the return slot: the export then returns a zero word, and the glue throws.
pub fn set_thrown(thrown: Thrown) {
    let (kind, msg) = match thrown {
        Thrown::Error(msg) => (thrown::ERROR, msg),
        Thrown::Value(msg) => (thrown::VALUE, msg),
    };
    let (ptr, len) = match msg.into_abi() {
        Ok((ptr, len)) => (ptr, len as usize),
        Err(_) => (1, 0),
    };
    RET.with(|s| {
        s.thrown.set(kind);
        s.msg_ptr.set(ptr);
        s.msg_len.set(len);
    });
}

/// The glue's half of a throw, for native tests.
#[must_use]
pub fn take_thrown() -> Option<Thrown> {
    let (kind, ptr, len) = RET.with(|s| {
        let kind = s.thrown.replace(thrown::NONE);
        (kind, s.msg_ptr.get(), s.msg_len.get())
    });
    if kind == thrown::NONE {
        return None;
    }
    let msg = if len == 0 {
        String::new()
    } else {
        // SAFETY: `set_thrown` handed out `len` bytes of UTF-8 at `ptr`, allocated at exactly that
        // length, which nothing has freed since.
        unsafe { String::from_raw_parts(ptr as *mut u8, len, len) }
    };
    Some(if kind == thrown::ERROR {
        Thrown::Error(msg)
    } else {
        Thrown::Value(msg)
    })
}

#[cfg(test)]
mod tests {
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

    /// A throw waits in the slot, at the offsets the glue reads, until it is
    /// taken once; taking it clears it.
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
    }
}
