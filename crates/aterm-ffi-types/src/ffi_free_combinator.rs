// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structurally-safe FFI free combinators.
//!
//! These functions enforce the correct sequence for freeing opaque FFI handles:
//!
//! 1. Null check (early return)
//! 2. Panic catch (unwind boundary)
//! 3. `mark_freed` via the selected [`FfiTracker`] (BEFORE deallocation — tracks pointer while live)
//! 4. Optional teardown closure (runs while the struct is still live)
//! 5. `Box::from_raw` + drop (actual deallocation)
//!
//! This ordering is critical: `mark_freed` MUST happen before `Box::from_raw`
//! so the pointer is tracked while the memory is still valid. The previous
//! pattern delegated tracking to caller closures, creating TOCTOU bugs when
//! callers called `mark_freed` after `Box::from_raw` (pointer already dangling).
//!
//! Part of #6577.

use crate::FfiErrorCode;
use crate::verification::FfiTracker;
use core::ffi::c_void;
use std::ffi::CString;

// ── Trust L0 deallocation helpers ────────────────────────────────────────
//
// The `unsafe` operations that used to sit inline in the combinators below
// live in these private, `#[inline(never)]` `unsafe fn` helpers instead. The
// combinators contain `catch_unwind` (via `aterm_ffi_catch_unwind!`), which
// the Trust full verifier cannot lower; a function whose MIR contains
// unmodeled `unsafe` operations must fully lower (hard error), while calls
// to crate-local, natively verified `unsafe fn`s are covered by their own
// proof evidence. Keeping the raw-pointer operations in helpers that
// themselves fully lower (no `catch_unwind`, no unmodeled std calls) lets
// both sides verify.

// Trust L0 note on the helpers below: the combinators' raw-pointer
// operations live in these private `#[inline(never)]` `unsafe fn`s rather
// than inline in the `catch_unwind` closures. Inline std-unsafe code (e.g.
// `Box::from_raw`, which MIR-inlines) pulls the whole FFI wrapper into the
// native full-verification lane, which can never lower `catch_unwind`; a
// local `#[inline(never)]` helper is the firewall that keeps the wrappers
// out of that lane. The helpers' own ay/L0 obligations are all proved
// (their operations are covered by Trust's unsafe model).

/// Deallocate a `Box`-allocated handle.
///
/// # Safety
///
/// `handle` must be a valid, non-null, unfreed pointer from `Box::into_raw`
/// — guaranteed at the call sites by the combinators' null check plus the
/// tracker's atomic `mark_freed` test-and-set gate.
#[inline(never)]
unsafe fn drop_boxed<H>(handle: *mut H) {
    // Trust L0: spelled as `drop_in_place` + `alloc::dealloc` — the exact
    // two operations `drop(Box::from_raw(handle))` performs for a sized
    // `Global` box — because those two calls are covered by Trust's unsafe
    // model while `Box::from_raw` is fail-closed unmodeled. One deliberate
    // difference: if `H`'s destructor unwinds, the memory is leaked rather
    // than freed (Box's drop glue would free it); the panic still reaches
    // the combinator's `catch_unwind`, and leaking on a panicking `Drop` is
    // memory-safe and strictly more conservative.
    //
    // Known residual (documented toolchain gap): this helper is the one
    // function in the crate the native full-verification lane still rejects
    // — dropping a generic user type (`drop_in_place::<H>` glue) is
    // fail-closed unlowerable by design, and no spelling of "deallocate a
    // `Box<H>`" avoids it. Every ay/L0 obligation of this helper is proved.
    //
    // SAFETY: forwarded contract — `handle` came from `Box::into_raw` (so it
    // is non-null, properly aligned, and owns a live `H` allocated with
    // `Layout::new::<H>()` under the `Global` allocator) and the atomic
    // freed-set gate ensures exactly one caller reaches this deallocation.
    unsafe {
        // SAFETY: `handle` points to a live, properly aligned `H` (from
        // `Box::into_raw`, never freed — see the forwarded contract above),
        // so dropping the pointee in place is sound.
        core::ptr::drop_in_place(handle);
        if core::mem::size_of::<H>() != 0 {
            // SAFETY: the allocation was made by `Box::new` with
            // `Layout::new::<H>()` under the `Global` allocator, is non-zero
            // sized on this branch, and is freed exactly once (atomic
            // freed-set gate) — matching `dealloc`'s contract.
            std::alloc::dealloc(handle.cast::<u8>(), core::alloc::Layout::new::<H>());
        }
    }
}

/// Deallocate a `CString`-allocated FFI string.
///
/// # Safety
///
/// `ptr` must be a valid, non-null, unfreed pointer from `CString::into_raw`
/// — guaranteed at the call sites by the combinator's null check plus the
/// tracker's atomic `mark_freed` test-and-set gate.
#[inline(never)]
unsafe fn drop_cstring(ptr: *mut std::os::raw::c_char) {
    // SAFETY: forwarded contract — `ptr` came from `CString::into_raw` and
    // the atomic freed-set gate ensures exactly one caller reaches this
    // deallocation.
    drop(unsafe { CString::from_raw(ptr) });
}

/// Read the inner handle out of a pointer-to-pointer.
///
/// # Safety
///
/// `handle_ptr` must be non-null and valid for reads (standard FFI contract
/// of `box_handle_free_v2_nulling`).
#[inline(never)]
unsafe fn read_inner_handle<H>(handle_ptr: *mut *mut H) -> *mut H {
    // SAFETY: forwarded contract — caller validated `handle_ptr` is non-null
    // and guarantees readable memory.
    unsafe { *handle_ptr }
}

/// Write null into the caller's handle slot (null-after-free).
///
/// # Safety
///
/// `handle_ptr` must be non-null and valid for writes (standard FFI contract
/// of `box_handle_free_v2_nulling`).
#[inline(never)]
unsafe fn null_inner_handle<H>(handle_ptr: *mut *mut H) {
    // SAFETY: forwarded contract — caller validated `handle_ptr` is non-null
    // and guarantees writable memory.
    unsafe { *handle_ptr = std::ptr::null_mut() };
}

/// Free a `Box`-allocated FFI handle (V2 — returns error code).
///
/// Sequence: null check → panic catch → `mark_freed` → `Box::from_raw` + drop.
///
/// Returns `E::double_free()` if the pointer was already freed.
///
/// # Safety
///
/// - `handle` must be null or a valid pointer from `Box::into_raw`.
/// - `handle` must not have been freed previously (detected, not UB).
pub unsafe fn box_handle_free_v2<H, E: FfiErrorCode>(
    name: &'static str,
    handle: *mut H,
    tracker: FfiTracker,
) -> E {
    // Trust L0: carry the tracker across the unwind-closure boundary as its
    // u8 tag — an enum-typed upvar trips unsupported `AggregateKind::Closure`
    // MIR in the Trust full verifier (see `FfiTracker::tag`). The roundtrip
    // is a proven identity, so behavior is unchanged.
    let tracker_tag = tracker.tag();
    crate::aterm_ffi_catch_unwind!(E::internal(), { /* panic caught at FFI boundary */ }, {
        let tracker = FfiTracker::from_tag(tracker_tag);
        if handle.is_null() {
            return E::null_handle();
        }
        if tracker.is_freed(handle.cast::<c_void>()) {
            return E::double_free();
        }
        if !tracker.is_allocated(handle.cast::<c_void>()) {
            aterm_log::error!("{}: rejecting free of untracked handle {:p}", name, handle);
            return E::internal();
        }
        // Atomic test-and-set: the tracker's single locked critical section is the
        // authoritative gate. The is_freed read above is a fast early-out, but two
        // racing threads can both pass it; exactly one of them gets `false` here and
        // proceeds to free. The loser sees `true` and returns without freeing,
        // closing the concurrent free-vs-free double-free window.
        if tracker.mark_freed(handle.cast::<c_void>()) {
            return E::double_free();
        }
        // SAFETY: caller guarantees `handle` is a valid pointer from
        // `Box::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null, tracked, and freed exactly once.
        unsafe { drop_boxed(handle) };
        E::ok()
    })
}

/// Free a `Box`-allocated FFI handle with pre-free teardown (V2 — returns error code).
///
/// Sequence: null check → panic catch → `mark_freed` → teardown → `Box::from_raw` + drop.
///
/// The `teardown` closure receives the raw pointer (guaranteed non-null, not yet
/// deallocated) and can perform cleanup such as disarming callback guards or
/// clearing magic canaries. The struct is still live during teardown.
///
/// Returns `E::double_free()` if the pointer was already freed.
///
/// # Safety
///
/// - `handle` must be null or a valid pointer from `Box::into_raw`.
/// - `handle` must not have been freed previously (detected, not UB).
/// - The `teardown` closure must not deallocate `handle`.
pub unsafe fn box_handle_free_v2_with_teardown<H, E, F>(
    name: &'static str,
    handle: *mut H,
    tracker: FfiTracker,
    teardown: F,
) -> E
where
    E: FfiErrorCode,
    F: FnOnce(*mut H),
{
    crate::aterm_ffi_catch_unwind!(E::internal(), { /* panic caught at FFI boundary */ }, {
        if handle.is_null() {
            return E::null_handle();
        }
        if tracker.is_freed(handle.cast::<c_void>()) {
            return E::double_free();
        }
        if !tracker.is_allocated(handle.cast::<c_void>()) {
            aterm_log::error!("{}: rejecting free of untracked handle {:p}", name, handle);
            return E::internal();
        }
        // Atomic test-and-set gate (see box_handle_free_v2): the racing loser
        // returns ErrDoubleFree and never runs teardown or frees.
        if tracker.mark_freed(handle.cast::<c_void>()) {
            return E::double_free();
        }
        teardown(handle);
        // SAFETY: caller guarantees `handle` is a valid pointer from
        // `Box::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null, tracked, and freed exactly once.
        unsafe { drop_boxed(handle) };
        E::ok()
    })
}

/// Free a `Box`-allocated FFI handle (V1 — void return).
///
/// Sequence: null check → panic catch → `assert_not_freed` → `mark_freed` → `Box::from_raw` + drop.
///
/// Panics (caught by unwind guard) on double-free via `assert_not_freed`.
///
/// # Safety
///
/// - `handle` must be null or a valid pointer from `Box::into_raw`.
/// - `handle` must not have been freed previously (panics on double-free).
pub unsafe fn box_handle_free_v1<H>(name: &'static str, handle: *mut H, tracker: FfiTracker) {
    crate::aterm_ffi_catch_unwind!((), { /* panic caught at FFI boundary */ }, {
        if handle.is_null() {
            return;
        }
        if tracker.is_freed(handle.cast::<c_void>()) {
            tracker.assert_not_freed(handle.cast::<c_void>());
        }
        if !tracker.is_allocated(handle.cast::<c_void>()) {
            aterm_log::error!("{}: rejecting free of untracked handle {:p}", name, handle);
            return;
        }
        tracker.assert_not_freed(handle.cast::<c_void>());
        // Atomic test-and-set is the authoritative gate. The is_freed/assert_not_freed
        // reads above are a fast early-out, but two racing threads can both pass them;
        // exactly one gets `false` here and proceeds to free. The loser sees `true` and
        // re-asserts (panicking, caught by the unwind guard) so v1's panic-on-double-free
        // semantics are preserved and it never reaches Box::from_raw.
        if tracker.mark_freed(handle.cast::<c_void>()) {
            tracker.assert_not_freed(handle.cast::<c_void>());
            return;
        }
        // SAFETY: caller guarantees `handle` is a valid pointer from
        // `Box::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null, tracked, and freed exactly once.
        unsafe { drop_boxed(handle) };
    });
}

/// Free a `Box`-allocated FFI handle with pre-free teardown (V1 — void return).
///
/// Sequence: null check → panic catch → `assert_not_freed` → `mark_freed` → teardown → `Box::from_raw` + drop.
///
/// Panics (caught by unwind guard) on double-free via `assert_not_freed`.
///
/// # Safety
///
/// - `handle` must be null or a valid pointer from `Box::into_raw`.
/// - `handle` must not have been freed previously (panics on double-free).
/// - The `teardown` closure must not deallocate `handle`.
pub unsafe fn box_handle_free_v1_with_teardown<H, F>(
    name: &'static str,
    handle: *mut H,
    tracker: FfiTracker,
    teardown: F,
) where
    F: FnOnce(*mut H),
{
    crate::aterm_ffi_catch_unwind!((), { /* panic caught at FFI boundary */ }, {
        if handle.is_null() {
            return;
        }
        if tracker.is_freed(handle.cast::<c_void>()) {
            tracker.assert_not_freed(handle.cast::<c_void>());
        }
        if !tracker.is_allocated(handle.cast::<c_void>()) {
            aterm_log::error!("{}: rejecting free of untracked handle {:p}", name, handle);
            return;
        }
        tracker.assert_not_freed(handle.cast::<c_void>());
        // Atomic test-and-set is the authoritative gate (see box_handle_free_v1):
        // the racing loser re-asserts (panicking, caught by the unwind guard) and
        // never runs teardown or reaches Box::from_raw.
        if tracker.mark_freed(handle.cast::<c_void>()) {
            tracker.assert_not_freed(handle.cast::<c_void>());
            return;
        }
        teardown(handle);
        // SAFETY: caller guarantees `handle` is a valid pointer from
        // `Box::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null, tracked, and freed exactly once.
        unsafe { drop_boxed(handle) };
    });
}

/// Free a `Box`-allocated FFI handle with caller-specified null error (V2).
///
/// Like [`box_handle_free_v2`] but the caller provides the null-handle error
/// value directly. This supports error types where `null_handle()` returns a
/// generic sentinel but individual functions need handle-specific null variants
/// (e.g., `AtermGpuError::ErrNullPathBuilder` vs `ErrNullCanvas`).
///
/// # Safety
///
/// - `handle` must be null or a valid pointer from `Box::into_raw`.
/// - `handle` must not have been freed previously (detected, not UB).
pub unsafe fn box_handle_free_v2_with_null<H, E: FfiErrorCode>(
    name: &'static str,
    handle: *mut H,
    tracker: FfiTracker,
    null_err: E,
) -> E {
    crate::aterm_ffi_catch_unwind!(E::internal(), { /* panic caught at FFI boundary */ }, {
        if handle.is_null() {
            return null_err;
        }
        if tracker.is_freed(handle.cast::<c_void>()) {
            return E::double_free();
        }
        if !tracker.is_allocated(handle.cast::<c_void>()) {
            aterm_log::error!("{}: rejecting free of untracked handle {:p}", name, handle);
            return E::internal();
        }
        // Atomic test-and-set gate (see box_handle_free_v2): the racing loser
        // returns ErrDoubleFree and never frees.
        if tracker.mark_freed(handle.cast::<c_void>()) {
            return E::double_free();
        }
        // SAFETY: caller guarantees `handle` is a valid pointer from
        // `Box::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null, tracked, and freed exactly once.
        unsafe { drop_boxed(handle) };
        E::ok()
    })
}

/// Free a `Box`-allocated FFI handle through a pointer-to-pointer with null-after-free (V2).
///
/// Takes `*mut *mut H` (pointer-to-pointer), reads the inner handle, frees it,
/// then writes null back to the caller's variable. This prevents use-after-free
/// because subsequent dereferences see null and get an error code, not UB.
///
/// Sequence: outer-null check → read inner → inner-null check → `mark_freed` → `Box::from_raw` → null-after-free.
///
/// # Safety
///
/// - `handle_ptr` must be null or a valid pointer to a `*mut H`.
/// - The inner `*handle_ptr` must be null or a valid pointer from `Box::into_raw`.
/// - The inner pointer must not have been freed previously (detected, not UB).
pub unsafe fn box_handle_free_v2_nulling<H, E: FfiErrorCode>(
    name: &'static str,
    handle_ptr: *mut *mut H,
    tracker: FfiTracker,
    null_err: E,
) -> E {
    crate::aterm_ffi_catch_unwind!(E::internal(), { /* panic caught at FFI boundary */ }, {
        if handle_ptr.is_null() {
            return null_err;
        }
        // SAFETY: `handle_ptr` was null-checked above and the caller
        // guarantees it is valid for reads/writes.
        let inner = unsafe { read_inner_handle(handle_ptr) };
        if inner.is_null() {
            return null_err;
        }
        if tracker.is_freed(inner.cast::<c_void>()) {
            return E::double_free();
        }
        if !tracker.is_allocated(inner.cast::<c_void>()) {
            aterm_log::error!("{}: rejecting free of untracked handle {:p}", name, inner);
            return E::internal();
        }
        // Atomic test-and-set gate (see box_handle_free_v2): the racing loser
        // returns ErrDoubleFree and never frees or nulls the caller's slot.
        if tracker.mark_freed(inner.cast::<c_void>()) {
            return E::double_free();
        }
        // SAFETY: caller guarantees `inner` is a valid pointer from
        // `Box::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null, tracked, and freed exactly once.
        unsafe { drop_boxed(inner) };
        // Null out the caller's handle to prevent use-after-free.
        // SAFETY: `handle_ptr` validated non-null above; caller guarantees
        // writable memory.
        unsafe { null_inner_handle(handle_ptr) };
        E::ok()
    })
}

/// Free a `CString`-allocated FFI string (V1 — void return).
///
/// Sequence: null check → panic catch → `assert_not_freed` → `mark_freed` → `CString::from_raw` + drop.
///
/// Panics (caught by unwind guard) on double-free via `assert_not_freed`.
///
/// # Safety
///
/// - `ptr` must be null or a valid pointer from `CString::into_raw`.
/// - `ptr` must not have been freed previously (panics on double-free).
pub unsafe fn cstring_handle_free_v1(
    _name: &'static str,
    ptr: *mut std::os::raw::c_char,
    tracker: FfiTracker,
) {
    crate::aterm_ffi_catch_unwind!((), { /* panic caught at FFI boundary */ }, {
        if ptr.is_null() {
            return;
        }
        tracker.assert_not_freed(ptr.cast::<c_void>());
        // Atomic test-and-set is the authoritative gate (see box_handle_free_v1):
        // the racing loser re-asserts (panicking, caught by the unwind guard) and
        // never reaches CString::from_raw.
        if tracker.mark_freed(ptr.cast::<c_void>()) {
            tracker.assert_not_freed(ptr.cast::<c_void>());
            return;
        }
        // SAFETY: caller guarantees `ptr` is a valid pointer from
        // `CString::into_raw`; the null check and atomic freed-set gate above
        // ensure it is non-null and freed exactly once.
        unsafe { drop_cstring(ptr) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification::ffi_free_tracker;

    #[test]
    fn box_free_v2_null_returns_null_handle() {
        let result: crate::AtermTerminalError = unsafe {
            box_handle_free_v2::<u64, _>("test", std::ptr::null_mut(), FfiTracker::General)
        };
        assert_eq!(result, crate::AtermTerminalError::ErrNullTerminal);
    }

    #[test]
    fn box_free_v2_valid_pointer_returns_ok() {
        let val = Box::into_raw(Box::new(42u64));
        ffi_free_tracker::mark_allocated(val.cast());

        let result: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::General) };
        assert_eq!(result, crate::AtermTerminalError::Ok);
    }

    #[test]
    fn box_free_v2_double_free_returns_error() {
        let val = Box::into_raw(Box::new(42u64));
        ffi_free_tracker::mark_allocated(val.cast());

        let first: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::General) };
        assert_eq!(first, crate::AtermTerminalError::Ok);

        // Second free on same pointer — should detect double-free.
        let second: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::General) };
        assert_eq!(second, crate::AtermTerminalError::ErrDoubleFree);
    }

    #[test]
    fn box_free_v2_with_teardown_runs_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let val = Box::into_raw(Box::new(42u64));
        ffi_free_tracker::mark_allocated(val.cast());
        let teardown_ran = AtomicBool::new(false);

        let result: crate::AtermTerminalError = unsafe {
            box_handle_free_v2_with_teardown("test", val, FfiTracker::General, |_ptr| {
                teardown_ran.store(true, Ordering::Relaxed);
            })
        };
        assert_eq!(result, crate::AtermTerminalError::Ok);
        assert!(teardown_ran.load(Ordering::Relaxed));
    }

    #[test]
    fn box_free_v1_null_is_noop() {
        unsafe { box_handle_free_v1::<u64>("test", std::ptr::null_mut(), FfiTracker::General) };
        // No panic — passes
    }

    #[test]
    fn box_free_v1_valid_pointer_succeeds() {
        let val = Box::into_raw(Box::new(99u64));
        ffi_free_tracker::mark_allocated(val.cast());

        unsafe { box_handle_free_v1("test", val, FfiTracker::General) };
        // Pointer is now freed
        assert!(ffi_free_tracker::is_freed(val.cast()));
    }

    #[test]
    fn cstring_free_v1_null_is_noop() {
        unsafe { cstring_handle_free_v1("test", std::ptr::null_mut(), FfiTracker::General) };
        // No panic — passes
    }

    #[test]
    fn cstring_free_v1_valid_pointer_succeeds() {
        let s = CString::new("hello").expect("valid CString");
        let ptr = s.into_raw();
        ffi_free_tracker::mark_allocated(ptr.cast());

        unsafe { cstring_handle_free_v1("test", ptr, FfiTracker::General) };
        assert!(ffi_free_tracker::is_freed(ptr.cast()));
    }

    // ── V1 double-free detection ───────────────────────────────────────

    #[test]
    fn box_free_v1_double_free_is_caught_by_unwind() {
        let val = Box::into_raw(Box::new(42u64));
        ffi_free_tracker::mark_allocated(val.cast());

        unsafe { box_handle_free_v1("test", val, FfiTracker::General) };
        // Second free: assert_not_freed panics, caught by catch_unwind — no crash.
        unsafe { box_handle_free_v1("test", val, FfiTracker::General) };
    }

    #[test]
    fn cstring_free_v1_double_free_is_caught_by_unwind() {
        let s = CString::new("world").expect("valid CString");
        let ptr = s.into_raw();
        ffi_free_tracker::mark_allocated(ptr.cast());

        unsafe { cstring_handle_free_v1("test", ptr, FfiTracker::General) };
        // Second free: assert_not_freed panics, caught by catch_unwind — no crash.
        unsafe { cstring_handle_free_v1("test", ptr, FfiTracker::General) };
    }

    // ── V1 with teardown ───────────────────────────────────────────────

    #[test]
    fn box_free_v1_with_teardown_runs_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let val = Box::into_raw(Box::new(77u64));
        ffi_free_tracker::mark_allocated(val.cast());
        let teardown_ran = AtomicBool::new(false);

        unsafe {
            box_handle_free_v1_with_teardown("test", val, FfiTracker::General, |_ptr| {
                teardown_ran.store(true, Ordering::Relaxed);
            });
        }
        assert!(teardown_ran.load(Ordering::Relaxed));
        assert!(ffi_free_tracker::is_freed(val.cast()));
    }

    #[test]
    fn box_free_v1_with_teardown_null_skips_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let teardown_ran = AtomicBool::new(false);
        unsafe {
            box_handle_free_v1_with_teardown::<u64, _>(
                "test",
                std::ptr::null_mut(),
                FfiTracker::General,
                |_ptr| {
                    teardown_ran.store(true, Ordering::Relaxed);
                },
            );
        }
        assert!(!teardown_ran.load(Ordering::Relaxed));
    }

    #[test]
    fn box_free_v1_with_teardown_double_free_skips_closure() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let val = Box::into_raw(Box::new(88u64));
        ffi_free_tracker::mark_allocated(val.cast());
        let call_count = AtomicU32::new(0);

        // First free: teardown runs.
        unsafe {
            box_handle_free_v1_with_teardown("test", val, FfiTracker::General, |_ptr| {
                call_count.fetch_add(1, Ordering::Relaxed);
            });
        }
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        // Second free: assert_not_freed panics before teardown — caught by unwind.
        unsafe {
            box_handle_free_v1_with_teardown("test", val, FfiTracker::General, |_ptr| {
                call_count.fetch_add(1, Ordering::Relaxed);
            });
        }
        // Teardown did NOT run on the second call.
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
    }

    // ── V2 with teardown edge cases ────────────────────────────────────

    #[test]
    fn box_free_v2_with_teardown_null_skips_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let teardown_ran = AtomicBool::new(false);
        let result: crate::AtermTerminalError = unsafe {
            box_handle_free_v2_with_teardown::<u64, _, _>(
                "test",
                std::ptr::null_mut(),
                FfiTracker::General,
                |_ptr| {
                    teardown_ran.store(true, Ordering::Relaxed);
                },
            )
        };
        assert_eq!(result, crate::AtermTerminalError::ErrNullTerminal);
        assert!(!teardown_ran.load(Ordering::Relaxed));
    }

    #[test]
    fn box_free_v2_with_teardown_double_free_skips_closure() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let val = Box::into_raw(Box::new(55u64));
        ffi_free_tracker::mark_allocated(val.cast());
        let call_count = AtomicU32::new(0);

        // First free: teardown runs.
        let first: crate::AtermTerminalError = unsafe {
            box_handle_free_v2_with_teardown("test", val, FfiTracker::General, |_ptr| {
                call_count.fetch_add(1, Ordering::Relaxed);
            })
        };
        assert_eq!(first, crate::AtermTerminalError::Ok);
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        // Second free: returns ErrDoubleFree, teardown NOT called.
        let second: crate::AtermTerminalError = unsafe {
            box_handle_free_v2_with_teardown("test", val, FfiTracker::General, |_ptr| {
                call_count.fetch_add(1, Ordering::Relaxed);
            })
        };
        assert_eq!(second, crate::AtermTerminalError::ErrDoubleFree);
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
    }

    // ── FfiTracker::Terminal path ──────────────────────────────────────

    #[test]
    fn box_free_v2_terminal_tracker_works() {
        use crate::verification::terminal_handle_tracker;

        let val = Box::into_raw(Box::new(123u64));
        terminal_handle_tracker::mark_allocated(val.cast());

        let result: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::Terminal) };
        assert_eq!(result, crate::AtermTerminalError::Ok);
        assert!(terminal_handle_tracker::is_freed(val.cast()));
    }

    // ── Atomic free gate (bug 12: mark_freed return must be consumed) ──

    /// Documents that the freed-set test-and-set, not the earlier `is_freed`
    /// read, is the authoritative gate. Single-threaded: the first free succeeds
    /// and the second is detected via `mark_freed` returning `true`.
    #[test]
    fn box_free_v2_gate_consumes_mark_freed_return() {
        let val = Box::into_raw(Box::new(7u64));
        ffi_free_tracker::mark_allocated(val.cast());

        // First free: mark_freed returns false → frees, returns Ok.
        let first: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::General) };
        assert_eq!(first, crate::AtermTerminalError::Ok);
        assert!(ffi_free_tracker::is_freed(val.cast()));

        // Second free: mark_freed returns true → ErrDoubleFree, no second drop.
        let second: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::General) };
        assert_eq!(second, crate::AtermTerminalError::ErrDoubleFree);
    }

    // NOTE: a live-thread concurrent free-vs-free test is intentionally NOT
    // included here. Exercising "exactly one thread frees" through the real
    // combinator would require two threads to race `box_handle_free_v2` on the
    // same `Box`; if the gate ever regressed, that test would itself trigger the
    // double-free UB it is meant to catch (and, against the process-global,
    // address-keyed tracker shared by every test in this module, real-`Box`
    // address reuse makes such a test flaky). The concurrent guarantee instead
    // holds by composition: `ffi_free_tracker::mark_freed` is an atomic
    // test-and-set inside a single locked critical section (verified by
    // `mark_freed_detects_double_free` and the tracker's Kani proof), and every
    // combinator above now returns on its `true` return BEFORE `Box::from_raw`,
    // so at most one racing caller can ever reach the free.

    #[test]
    fn box_free_v2_untracked_pointer_returns_internal_without_freeing() {
        let anchor = 123u64;
        let val = std::ptr::addr_of!(anchor).cast_mut();

        let result: crate::AtermTerminalError =
            unsafe { box_handle_free_v2("test", val, FfiTracker::General) };
        assert_eq!(result, crate::AtermTerminalError::ErrInternal);
    }
}
