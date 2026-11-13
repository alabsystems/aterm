// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

std::thread_local! {
    static MS: Cell<u64> = const { Cell::new(0) };
}

/// Stall the child of each spawn this thread makes by `ms` before its exec (0: none).
pub(crate) fn set(ms: u64) {
    MS.with(|m| m.set(ms));
}

/// In the child, between fork and exec: a const thread-local read (the
/// child's one thread is the copy of the thread that forked it, so it
/// reads that thread's value, with no allocation or lock) and `nanosleep`.
pub(super) fn in_child() {
    let ms = MS.with(Cell::get);
    if ms == 0 {
        return;
    }
    let ts = libc::timespec {
        tv_sec: libc::time_t::try_from(ms / 1000).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::try_from((ms % 1000) * 1_000_000).unwrap_or(0),
    };
    // SAFETY: `ts` is an initialized `timespec` and the remainder pointer is null.
    unsafe {
        libc::nanosleep(&ts, std::ptr::null_mut());
    }
}
