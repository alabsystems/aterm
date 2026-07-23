// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal-handle-specific free-tracking for double-free and use-after-free detection.
//!
//! Both this tracker and the generic `ffi_free_tracker` are **always active** in
//! non-Kani builds. This tracker is scoped to long-lived `AtermTerminal*` handles
//! only, keeping the tracked population bounded to terminal roots rather than
//! every transient FFI allocation.
//!
//! History: the generic tracker previously compiled to no-ops in release builds,
//! which caused `aterm_terminal_free_v2` to double-drop and
//! `aterm_terminal_process_v2` to dereference freed memory (#5856). Both trackers
//! are now always-on.

#[cfg(kani)]
mod inner {
    use core::ffi::c_void;

    const MAX_TRACKED: usize = 64;
    static mut FREED: [usize; MAX_TRACKED] = [0; MAX_TRACKED];
    static mut FREED_LEN: usize = 0;
    static mut ALLOCATED: [usize; MAX_TRACKED] = [0; MAX_TRACKED];
    static mut ALLOCATED_LEN: usize = 0;

    // Edition 2024 makes `&FREED` / `&mut ALLOCATED_LEN` (references into a
    // `static mut`) a hard error, so every access below goes through raw pointers.
    // `&raw mut`/`&raw const` take an address without forming a reference; the
    // arrays are contiguous, so `.cast()` + `.add()` index them exactly as
    // `FREED[i]` / `FREED_LEN` did.

    /// SAFETY: `entries` must point to at least `len` consecutive valid `usize`.
    unsafe fn contains(entries: *const usize, len: usize, addr: usize) -> bool {
        for idx in 0..len {
            // SAFETY: idx < len; the caller guarantees `len` valid elements.
            if unsafe { entries.add(idx).read() } == addr {
                return true;
            }
        }
        false
    }

    /// SAFETY: `entries` must point to at least `*len` consecutive valid `usize`,
    /// and `len` to a valid `usize`.
    unsafe fn remove(entries: *mut usize, len: *mut usize, addr: usize) {
        // SAFETY: caller guarantees `len` is valid.
        let n = unsafe { len.read() };
        for idx in 0..n {
            // SAFETY: idx < n <= number of valid elements.
            if unsafe { entries.add(idx).read() } == addr {
                // SAFETY: n >= 1 in this branch, so n - 1 is a valid index.
                let last = unsafe { entries.add(n - 1).read() };
                // SAFETY: idx < n is a valid element to overwrite.
                unsafe { entries.add(idx).write(last) };
                // SAFETY: caller guarantees `len` is valid.
                unsafe { len.write(n - 1) };
                return;
            }
        }
    }

    /// Check if a terminal handle has been recorded as freed.
    pub fn is_freed(ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let addr = ptr as usize;
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { (&raw const FREED_LEN).read() };
        // SAFETY: Single-threaded Kani execution; no data race possible.
        unsafe { contains((&raw const FREED).cast::<usize>(), len, addr) }
    }

    /// Check if a terminal handle is currently recorded as live.
    pub fn is_allocated(ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let addr = ptr as usize;
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { (&raw const ALLOCATED_LEN).read() };
        // SAFETY: Single-threaded Kani execution; no data race possible.
        unsafe { contains((&raw const ALLOCATED).cast::<usize>(), len, addr) }
    }

    /// Record a terminal handle as freed. Returns `true` if already freed (double-free).
    pub fn mark_freed(ptr: *mut c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        if is_freed(ptr as *const c_void) {
            return true;
        }
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { (&raw const FREED_LEN).read() };
        kani::assert(
            len < MAX_TRACKED,
            "terminal_handle_tracker overflow (increase MAX_TRACKED)",
        );
        // SAFETY: Bounds checked by kani::assert above; single-threaded.
        unsafe {
            remove(
                (&raw mut ALLOCATED).cast::<usize>(),
                &raw mut ALLOCATED_LEN,
                ptr as usize,
            );
            (&raw mut FREED)
                .cast::<usize>()
                .add(len)
                .write(ptr as usize);
            (&raw mut FREED_LEN).write(len + 1);
        }
        false
    }

    /// Assert (via Kani) that a terminal handle has not been freed.
    pub fn assert_not_freed(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        kani::assert(
            !is_freed(ptr as *const c_void),
            "double-free detected by terminal_handle_tracker",
        );
    }

    /// Remove a terminal handle from the freed set (e.g., after reallocation).
    pub fn mark_allocated(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let addr = ptr as usize;
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { (&raw const ALLOCATED_LEN).read() };
        kani::assert(
            len < MAX_TRACKED || is_allocated(ptr.cast_const()),
            "terminal_handle_tracker allocated-set overflow (increase MAX_TRACKED)",
        );
        // SAFETY: Single-threaded Kani execution; no data race possible.
        unsafe {
            remove((&raw mut FREED).cast::<usize>(), &raw mut FREED_LEN, addr);
            let alloc_len = (&raw const ALLOCATED_LEN).read();
            if !contains((&raw const ALLOCATED).cast::<usize>(), alloc_len, addr) {
                (&raw mut ALLOCATED)
                    .cast::<usize>()
                    .add(alloc_len)
                    .write(addr);
                (&raw mut ALLOCATED_LEN).write(alloc_len + 1);
            }
        }
    }
}

#[cfg(not(kani))]
mod inner {
    use core::ffi::c_void;
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    use aterm_types::MutexExt;

    #[derive(Default)]
    struct TrackerState {
        allocated: HashSet<usize>,
        freed: HashSet<usize>,
    }

    fn tracker_state() -> &'static Mutex<TrackerState> {
        static STATE: OnceLock<Mutex<TrackerState>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(TrackerState::default()))
    }

    /// Record a terminal handle as freed. Returns `true` if already freed (double-free).
    pub fn mark_freed(ptr: *mut c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let addr = ptr as usize;
        let mut state = tracker_state().lock_or_recover();
        if state.freed.contains(&addr) {
            return true;
        }
        state.allocated.remove(&addr);
        state.freed.insert(addr);
        false
    }

    /// Record a terminal handle as live and clear any stale freed-state.
    pub fn mark_allocated(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let addr = ptr as usize;
        let mut state = tracker_state().lock_or_recover();
        state.freed.remove(&addr);
        state.allocated.insert(addr);
    }

    /// Check if a terminal handle has been recorded as freed.
    pub fn is_freed(ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let addr = ptr as usize;
        let state = tracker_state().lock_or_recover();
        state.freed.contains(&addr)
    }

    /// Check if a terminal handle is currently recorded as live.
    pub fn is_allocated(ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let addr = ptr as usize;
        let state = tracker_state().lock_or_recover();
        state.allocated.contains(&addr)
    }

    /// Assert that a terminal handle has not been freed. Panics on double-free.
    pub fn assert_not_freed(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        assert!(
            !is_freed(ptr as *const c_void),
            "double-free detected by terminal_handle_tracker"
        );
    }
}

pub use inner::{assert_not_freed, is_allocated, is_freed, mark_allocated, mark_freed};

#[cfg(kani)]
mod proofs {
    use super::*;
    use core::ffi::c_void;

    /// Proof: `mark_freed` returns `true` (double-free) on a second call with the same pointer.
    #[kani::proof]
    #[kani::unwind(3)]
    fn mark_freed_detects_double_free() {
        let addr: usize = kani::any();
        kani::assume(addr != 0);
        let ptr = addr as *mut c_void;

        let first = mark_freed(ptr);
        kani::assert(
            !first,
            "first mark_freed must return false (not a double-free)",
        );

        let second = mark_freed(ptr);
        kani::assert(
            second,
            "second mark_freed must return true (double-free detected)",
        );
    }

    /// Proof: `assert_not_freed` panics after a pointer is marked freed.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::should_panic]
    fn assert_not_freed_catches_use_after_free() {
        let addr: usize = kani::any();
        kani::assume(addr != 0);
        let ptr = addr as *mut c_void;

        mark_freed(ptr);
        assert_not_freed(ptr); // must panic
    }

    /// Proof: `is_freed` is false for a pointer that was never marked.
    #[kani::proof]
    #[kani::unwind(3)]
    fn unmarked_pointer_is_not_freed() {
        let addr: usize = kani::any();
        kani::assume(addr != 0);
        let ptr = addr as *const c_void;
        kani::assert(!is_freed(ptr), "never-marked pointer must not be freed");
    }

    // null_is_never_freed removed (#5887): pure null-guard check duplicated
    // by the unit test `null_pointer_is_never_freed` below.
}

#[cfg(all(test, not(kani)))]
mod tests {
    use super::*;
    use core::ffi::c_void;

    #[test]
    fn null_pointer_is_never_freed() {
        assert!(!is_freed(std::ptr::null::<c_void>()));
        assert!(!is_allocated(std::ptr::null::<c_void>()));
        assert!(!mark_freed(std::ptr::null_mut::<c_void>()));
        mark_allocated(std::ptr::null_mut::<c_void>());
        assert!(!is_freed(std::ptr::null::<c_void>()));
    }
}
