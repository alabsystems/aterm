// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structurally-safe FFI free combinator.
//!
//! [`box_handle_free_v1`] enforces the correct sequence for freeing an opaque
//! FFI handle:
//!
//! 1. Null check (early return)
//! 2. Panic catch (unwind boundary)
//! 3. `mark_freed` via the selected [`FfiTracker`] (BEFORE deallocation — tracks pointer while live)
//! 4. `Box::from_raw` + drop (actual deallocation)
//!
//! This ordering is critical: `mark_freed` MUST happen before `Box::from_raw`
//! so the pointer is tracked while the memory is still valid. The previous
//! pattern delegated tracking to caller closures, creating TOCTOU bugs when
//! callers called `mark_freed` after `Box::from_raw` (pointer already dangling).
//!
//! Part of #6577.

use crate::verification::FfiTracker;
use core::ffi::c_void;

// ── Trust L0 deallocation helpers ────────────────────────────────────────
//
// The `unsafe` operations that used to sit inline in the combinator below
// live in this private, `#[inline(never)]` `unsafe fn` helper instead. The
// combinator contains `catch_unwind` (via `aterm_ffi_catch_unwind!`), which
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification::ffi_free_tracker;

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

    // ── V1 double-free detection ───────────────────────────────────────

    #[test]
    fn box_free_v1_double_free_is_caught_by_unwind() {
        let val = Box::into_raw(Box::new(42u64));
        ffi_free_tracker::mark_allocated(val.cast());

        unsafe { box_handle_free_v1("test", val, FfiTracker::General) };
        // Second free: assert_not_freed panics, caught by catch_unwind — no crash.
        unsafe { box_handle_free_v1("test", val, FfiTracker::General) };
    }

    // NOTE: a live-thread concurrent free-vs-free test is intentionally NOT
    // included here. Exercising "exactly one thread frees" through the real
    // combinator would require two threads to race `box_handle_free_v1` on the
    // same `Box`; if the gate ever regressed, that test would itself trigger the
    // double-free UB it is meant to catch (and, against the process-global,
    // address-keyed tracker shared by every test in this module, real-`Box`
    // address reuse makes such a test flaky). The concurrent guarantee instead
    // holds by composition: `ffi_free_tracker::mark_freed` is an atomic
    // test-and-set inside a single locked critical section (verified by
    // `mark_freed_detects_double_free` and the tracker's Kani proof), and the
    // combinator above returns on its `true` return BEFORE `Box::from_raw`, so
    // at most one racing caller can ever reach the free.
}
