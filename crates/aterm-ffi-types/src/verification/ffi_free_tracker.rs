// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unified FFI pointer free-tracking for double-free and use-after-free detection.
//!
//! Two compile-time implementations:
//! - **Kani** (`#[cfg(kani)]`): deterministic static array for formal verification
//! - **Runtime** (all non-Kani builds): `HashSet` + `Mutex` for always-on detection
//!
//! Previously had a release no-op, but #5856 showed that release no-ops on
//! `terminal_handle_tracker` caused real double-free and use-after-free bugs.
//! The same risk applies to all non-terminal handles (Grid, Capability, MCP,
//! GPU renderer, etc.) — their populations are bounded and the tracking overhead
//! is negligible. Always-on tracking is now uniform across all handle types.
//!
//! Replaces the previously separate `kani_free` and `free_guard` systems that
//! shared zero state, causing Kani proofs to always fail and double-free
//! assertions to be masked. See #4705.

const TRACKER_BUCKETS: usize = 7;

#[cfg(kani)]
mod inner {
    use super::TRACKER_BUCKETS;
    use core::ffi::c_void;

    const MAX_TRACKED: usize = 64;
    static mut FREED: [[usize; MAX_TRACKED]; TRACKER_BUCKETS] = [[0; MAX_TRACKED]; TRACKER_BUCKETS];
    static mut FREED_LEN: [usize; TRACKER_BUCKETS] = [0; TRACKER_BUCKETS];
    static mut ALLOCATED: [[usize; MAX_TRACKED]; TRACKER_BUCKETS] =
        [[0; MAX_TRACKED]; TRACKER_BUCKETS];
    static mut ALLOCATED_LEN: [usize; TRACKER_BUCKETS] = [0; TRACKER_BUCKETS];

    // Edition 2024 makes `&FREED[bucket]` / `&mut ALLOCATED_LEN[bucket]` (references
    // into a `static mut`) a hard error, so every access below goes through raw
    // pointers. `&raw mut`/`&raw const` take an address without forming a reference;
    // callers pass a `clamp_bucket`-clamped `bucket < TRACKER_BUCKETS`, so each
    // computed element/row pointer stays within the static's allocation. The arrays
    // are contiguous (row-major), so `.cast()` + `.add()` index them exactly as
    // `FREED[bucket]` / `FREED[bucket][i]` / `FREED_LEN[bucket]` did.

    /// Raw pointer to `FREED[bucket][0]`.
    fn freed_row(bucket: usize) -> *mut usize {
        // SAFETY: bucket < TRACKER_BUCKETS, so the row is in bounds of FREED.
        unsafe {
            (&raw mut FREED)
                .cast::<[usize; MAX_TRACKED]>()
                .add(bucket)
                .cast::<usize>()
        }
    }

    /// Raw pointer to `ALLOCATED[bucket][0]`.
    fn allocated_row(bucket: usize) -> *mut usize {
        // SAFETY: bucket < TRACKER_BUCKETS, so the row is in bounds of ALLOCATED.
        unsafe {
            (&raw mut ALLOCATED)
                .cast::<[usize; MAX_TRACKED]>()
                .add(bucket)
                .cast::<usize>()
        }
    }

    /// Raw pointer to `FREED_LEN[bucket]`.
    fn freed_len_ptr(bucket: usize) -> *mut usize {
        // SAFETY: bucket < TRACKER_BUCKETS, so the element is in bounds of FREED_LEN.
        unsafe { (&raw mut FREED_LEN).cast::<usize>().add(bucket) }
    }

    /// Raw pointer to `ALLOCATED_LEN[bucket]`.
    fn allocated_len_ptr(bucket: usize) -> *mut usize {
        // SAFETY: bucket < TRACKER_BUCKETS, so the element is in bounds of ALLOCATED_LEN.
        unsafe { (&raw mut ALLOCATED_LEN).cast::<usize>().add(bucket) }
    }

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

    fn clamp_bucket(bucket: usize) -> usize {
        if bucket < TRACKER_BUCKETS { bucket } else { 0 }
    }

    /// Check if a pointer address has been recorded as freed.
    pub fn is_freed_in(bucket: usize, ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let bucket = clamp_bucket(bucket);
        let addr = ptr as usize;
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { freed_len_ptr(bucket).read() };
        // SAFETY: Single-threaded Kani execution; no data race possible.
        unsafe { contains(freed_row(bucket), len, addr) }
    }

    /// Check if a pointer address is currently recorded as live.
    pub fn is_allocated_in(bucket: usize, ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let bucket = clamp_bucket(bucket);
        let addr = ptr as usize;
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { allocated_len_ptr(bucket).read() };
        // SAFETY: Single-threaded Kani execution; no data race possible.
        unsafe { contains(allocated_row(bucket), len, addr) }
    }

    /// Check whether a pointer is recorded as allocated in any tracker bucket.
    pub fn is_allocated_any(ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        for bucket in 0..TRACKER_BUCKETS {
            if is_allocated_in(bucket, ptr) {
                return true;
            }
        }
        false
    }

    /// Record a pointer address as freed. Returns `true` if already freed (double-free).
    pub fn mark_freed_in(bucket: usize, ptr: *mut c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let bucket = clamp_bucket(bucket);
        if is_freed_in(bucket, ptr as *const c_void) {
            return true;
        }
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { freed_len_ptr(bucket).read() };
        kani::assert(
            len < MAX_TRACKED,
            "ffi_free_tracker overflow (increase MAX_TRACKED)",
        );
        // SAFETY: Bounds checked by kani::assert above; single-threaded.
        unsafe {
            remove(
                allocated_row(bucket),
                allocated_len_ptr(bucket),
                ptr as usize,
            );
            freed_row(bucket).add(len).write(ptr as usize);
            freed_len_ptr(bucket).write(len + 1);
        }
        false
    }

    /// Assert (via Kani) that a pointer has not been freed. No-op outside Kani.
    pub fn assert_not_freed_in(bucket: usize, ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        kani::assert(
            !is_freed_in(bucket, ptr as *const c_void),
            "double-free detected by ffi_free_tracker",
        );
    }

    /// Remove a pointer from the freed set (e.g., after reallocation).
    pub fn mark_allocated_in(bucket: usize, ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let bucket = clamp_bucket(bucket);
        let addr = ptr as usize;
        // SAFETY: Single-threaded Kani execution; no data race possible.
        let len = unsafe { allocated_len_ptr(bucket).read() };
        kani::assert(
            len < MAX_TRACKED || is_allocated_in(bucket, ptr.cast_const()),
            "ffi_free_tracker allocated-set overflow (increase MAX_TRACKED)",
        );
        // SAFETY: Single-threaded Kani execution; no data race possible.
        unsafe {
            remove(freed_row(bucket), freed_len_ptr(bucket), addr);
            let alloc_len = allocated_len_ptr(bucket).read();
            if !contains(allocated_row(bucket), alloc_len, addr) {
                allocated_row(bucket).add(alloc_len).write(addr);
                allocated_len_ptr(bucket).write(alloc_len + 1);
            }
        }
    }

    /// Check whether a pointer was recorded as freed in the default bucket.
    pub fn is_freed(ptr: *const c_void) -> bool {
        is_freed_in(0, ptr)
    }

    /// Check whether a pointer is recorded as allocated in the default bucket.
    pub fn is_allocated(ptr: *const c_void) -> bool {
        is_allocated_in(0, ptr)
    }

    /// Mark a pointer as freed in the default bucket.
    ///
    /// Returns `true` when the pointer was already marked freed.
    pub fn mark_freed(ptr: *mut c_void) -> bool {
        mark_freed_in(0, ptr)
    }

    /// Assert that a pointer in the default bucket has not been freed.
    pub fn assert_not_freed(ptr: *mut c_void) {
        assert_not_freed_in(0, ptr)
    }

    /// Mark a pointer as allocated in the default bucket.
    pub fn mark_allocated(ptr: *mut c_void) {
        mark_allocated_in(0, ptr)
    }
}

#[cfg(not(kani))]
mod inner {
    use core::ffi::c_void;
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    use aterm_types::MutexExt;

    use super::TRACKER_BUCKETS;

    #[derive(Default)]
    struct TrackerState {
        allocated: HashSet<usize>,
        freed: HashSet<usize>,
    }

    fn tracker_state() -> &'static Mutex<Vec<TrackerState>> {
        static STATE: OnceLock<Mutex<Vec<TrackerState>>> = OnceLock::new();
        STATE.get_or_init(|| {
            Mutex::new(
                (0..TRACKER_BUCKETS)
                    .map(|_| TrackerState::default())
                    .collect(),
            )
        })
    }

    /// Record a pointer address as freed. Returns `true` if already freed (double-free).
    pub fn mark_freed_in(bucket: usize, ptr: *mut c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let bucket = bucket.min(TRACKER_BUCKETS - 1);
        let addr = ptr as usize;
        let mut state = tracker_state().lock_or_recover();
        let bucket_state = &mut state[bucket];
        if bucket_state.freed.contains(&addr) {
            return true;
        }
        bucket_state.allocated.remove(&addr);
        bucket_state.freed.insert(addr);
        false
    }

    /// Record a pointer as live and clear any stale freed-state.
    pub fn mark_allocated_in(bucket: usize, ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let bucket = bucket.min(TRACKER_BUCKETS - 1);
        let addr = ptr as usize;
        let mut state = tracker_state().lock_or_recover();
        let bucket_state = &mut state[bucket];
        bucket_state.freed.remove(&addr);
        bucket_state.allocated.insert(addr);
    }

    /// Check if a pointer address has been recorded as freed.
    pub fn is_freed_in(bucket: usize, ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let bucket = bucket.min(TRACKER_BUCKETS - 1);
        let addr = ptr as usize;
        let state = tracker_state().lock_or_recover();
        state[bucket].freed.contains(&addr)
    }

    /// Check if a pointer address is currently recorded as live.
    pub fn is_allocated_in(bucket: usize, ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let bucket = bucket.min(TRACKER_BUCKETS - 1);
        let addr = ptr as usize;
        let state = tracker_state().lock_or_recover();
        state[bucket].allocated.contains(&addr)
    }

    /// Check whether a pointer is recorded as live in any tracker bucket.
    pub fn is_allocated_any(ptr: *const c_void) -> bool {
        if ptr.is_null() {
            return false;
        }
        let addr = ptr as usize;
        let state = tracker_state().lock_or_recover();
        state
            .iter()
            .any(|bucket_state| bucket_state.allocated.contains(&addr))
    }

    /// Assert that a pointer has not been freed. Panics on double-free.
    pub fn assert_not_freed_in(bucket: usize, ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        assert!(
            !is_freed_in(bucket, ptr as *const c_void),
            "double-free detected by ffi_free_tracker"
        );
    }

    /// Check whether a pointer was recorded as freed in the default bucket.
    pub fn is_freed(ptr: *const c_void) -> bool {
        is_freed_in(0, ptr)
    }

    /// Check whether a pointer is recorded as allocated in the default bucket.
    pub fn is_allocated(ptr: *const c_void) -> bool {
        is_allocated_in(0, ptr)
    }

    /// Mark a pointer as freed in the default bucket.
    ///
    /// Returns `true` when the pointer was already marked freed.
    pub fn mark_freed(ptr: *mut c_void) -> bool {
        mark_freed_in(0, ptr)
    }

    /// Assert that a pointer in the default bucket has not been freed.
    pub fn assert_not_freed(ptr: *mut c_void) {
        assert_not_freed_in(0, ptr)
    }

    /// Mark a pointer as allocated in the default bucket.
    pub fn mark_allocated(ptr: *mut c_void) {
        mark_allocated_in(0, ptr)
    }
}

pub use inner::{
    assert_not_freed, assert_not_freed_in, is_allocated, is_allocated_any, is_allocated_in,
    is_freed, is_freed_in, mark_allocated, mark_allocated_in, mark_freed, mark_freed_in,
};

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

    #[test]
    fn mark_freed_returns_false_first_true_second() {
        // Use a unique address unlikely to collide with other tests.
        let val = Box::new(42u64);
        let ptr = Box::into_raw(val) as *mut c_void;

        // Clean slate
        mark_allocated(ptr);
        assert!(is_allocated(ptr as *const c_void));
        assert!(!is_freed(ptr as *const c_void));

        // First free: returns false (not a double-free)
        assert!(!mark_freed(ptr));
        // Now it's freed
        assert!(is_freed(ptr as *const c_void));
        assert!(!is_allocated(ptr as *const c_void));

        // Second free: returns true (double-free detected)
        assert!(mark_freed(ptr));

        // Clean up: re-mark as allocated, then actually free the Box
        mark_allocated(ptr);
        drop(unsafe { Box::from_raw(ptr as *mut u64) });
    }

    #[test]
    fn mark_allocated_clears_freed_state() {
        let val = Box::new(99u64);
        let ptr = Box::into_raw(val) as *mut c_void;

        mark_allocated(ptr);
        assert!(is_allocated(ptr as *const c_void));
        assert!(!mark_freed(ptr));
        assert!(is_freed(ptr as *const c_void));
        assert!(!is_allocated(ptr as *const c_void));

        // Re-allocate should clear the freed state
        mark_allocated(ptr);
        assert!(!is_freed(ptr as *const c_void));
        assert!(is_allocated(ptr as *const c_void));

        // Freeing again should return false (not double-free, since we re-allocated)
        assert!(!mark_freed(ptr));

        // Clean up
        mark_allocated(ptr);
        drop(unsafe { Box::from_raw(ptr as *mut u64) });
    }

    #[test]
    #[should_panic(expected = "double-free detected by ffi_free_tracker")]
    fn assert_not_freed_panics_on_freed_handle() {
        // Use a stack variable's address as a unique, stable pointer. The
        // tracker only compares address values (usize) and never dereferences.
        // Previous version used Box::new + drop, but the freed heap address
        // could be reused by concurrent tests whose mark_allocated calls
        // would clear our freed state (non-deterministic failure).
        let anchor = 77u64;
        let ptr = std::ptr::addr_of!(anchor) as *mut c_void;

        mark_allocated(ptr);
        mark_freed(ptr);
        assert_not_freed(ptr); // should panic
    }
}
