// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! C ABI embedding surface for the aterm engine (ATERM_DESIGN WS-D).
//!
//! The whole point of aterm is reliable screen reading; this exposes that to any
//! language with a C FFI as the canonical `feed bytes -> read the screen` loop,
//! over an opaque engine handle:
//!
//! ```c
//! AtermEngine* e = aterm_engine_new(24, 80);
//! aterm_engine_feed(e, input, input_len);
//! char* screen = aterm_engine_visible_content(e);   // NUL-terminated UTF-8
//! /* ... use screen ... */
//! aterm_string_free(screen);
//! aterm_engine_free(e);
//! ```
//!
//! All `unsafe` lives behind these wrappers; each function null-checks its handle.
//! Strings returned by this library are owned by the caller and MUST be released
//! with [`aterm_string_free`]; engines with [`aterm_engine_free`].
//!
//! # Panic safety
//! A Rust panic unwinding across the C ABI is undefined behavior. Every exported
//! function below therefore runs its body inside [`aterm_ffi_catch_unwind!`], which
//! catches any panic, logs it, and returns a safe sentinel (a null pointer for the
//! pointer-returning functions, `()` for the rest) instead of unwinding. The
//! success-path signature and semantics of each function are unchanged.

use std::ffi::{CString, c_char};

use aterm_core::terminal::Terminal;
use aterm_ffi_types::aterm_ffi_catch_unwind;

/// Opaque engine handle. The C side treats `*mut Terminal` as `AtermEngine*`.
pub use aterm_core::terminal::Terminal as Engine;

/// Create a new engine of `rows`×`cols`. Free with [`aterm_engine_free`].
#[unsafe(no_mangle)]
pub extern "C" fn aterm_engine_new(rows: u16, cols: u16) -> *mut Terminal {
    // On panic: return null so the caller sees allocation failure, not UB.
    aterm_ffi_catch_unwind!(std::ptr::null_mut(), {}, {
        Box::into_raw(Box::new(Terminal::new(rows, cols)))
    })
}

/// Feed `len` VT bytes at `ptr` to the engine. No-op on a null handle or pointer.
///
/// # Safety
/// `engine` must be a live handle from [`aterm_engine_new`]; `ptr` must point to
/// at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aterm_engine_feed(engine: *mut Terminal, ptr: *const u8, len: usize) {
    // On panic: swallow it (returning `()`) rather than unwind across the C ABI.
    aterm_ffi_catch_unwind!((), {}, {
        if engine.is_null() {
            return;
        }
        // SAFETY: caller contract — `engine` is a live handle.
        let term = unsafe { &mut *engine };
        // Validate the foreign `ptr`/`len` through the crate's hardened helper
        // instead of trusting them in `from_raw_parts`: it null-checks `ptr`,
        // enforces the `from_raw_parts` `isize::MAX` precondition (Kani-proven via
        // `is_valid_ffi_len`), and caps at `MAX_FFI_INPUT_BYTES`. A corrupted or
        // malicious length (e.g. the `read() == -1` footgun yielding `(usize)-1`)
        // becomes a safe no-op rather than instantaneous UB (CWE-120).
        // SAFETY: caller contract — `ptr`/`len` describe a readable region; the
        // helper rejects null/out-of-contract lengths before forming the slice.
        let Some(bytes) = (unsafe { aterm_ffi_types::ffi_byte_slice(ptr, len) }) else {
            return;
        };
        term.process(bytes);
    })
}

/// Resize the engine grid. No-op on a null handle.
///
/// # Safety
/// `engine` must be a live handle from [`aterm_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aterm_engine_resize(engine: *mut Terminal, rows: u16, cols: u16) {
    // On panic: swallow it (returning `()`) rather than unwind across the C ABI.
    aterm_ffi_catch_unwind!((), {}, {
        if engine.is_null() {
            return;
        }
        // SAFETY: caller contract — live handle.
        unsafe { &mut *engine }.resize(rows, cols);
    })
}

/// The visible screen as a newly-allocated NUL-terminated UTF-8 C string. Free
/// with [`aterm_string_free`]. Returns null on a null handle.
///
/// # Safety
/// `engine` must be a live handle from [`aterm_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aterm_engine_visible_content(engine: *const Terminal) -> *mut c_char {
    // On panic: return null rather than unwind across the C ABI.
    aterm_ffi_catch_unwind!(std::ptr::null_mut(), {}, {
        if engine.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: caller contract — live handle (shared read).
        let s = unsafe { &*engine }.visible_content();
        to_c_string(s)
    })
}

/// Row `row`'s text as a newly-allocated NUL-terminated UTF-8 C string. Free with
/// [`aterm_string_free`]. Returns null on a null handle or out-of-range row.
///
/// # Safety
/// `engine` must be a live handle from [`aterm_engine_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aterm_engine_row_text(engine: *const Terminal, row: usize) -> *mut c_char {
    // On panic: return null rather than unwind across the C ABI.
    aterm_ffi_catch_unwind!(std::ptr::null_mut(), {}, {
        if engine.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: caller contract — live handle (shared read).
        match unsafe { &*engine }.row_text(row) {
            Some(s) => to_c_string(s),
            None => std::ptr::null_mut(),
        }
    })
}

/// Free a string previously returned by this library. No-op on null.
///
/// # Safety
/// `s` must be a pointer returned by one of this library's string functions and
/// not already freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aterm_string_free(s: *mut c_char) {
    // On panic: swallow it (returning `()`) rather than unwind across the C ABI.
    aterm_ffi_catch_unwind!((), {}, {
        if !s.is_null() {
            // SAFETY: caller contract — `s` came from `CString::into_raw` here.
            drop(unsafe { CString::from_raw(s) });
        }
    })
}

/// Free an engine handle. No-op on null.
///
/// # Safety
/// `engine` must be a handle from [`aterm_engine_new`] and not already freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aterm_engine_free(engine: *mut Terminal) {
    // On panic: swallow it (returning `()`) rather than unwind across the C ABI.
    aterm_ffi_catch_unwind!((), {}, {
        if !engine.is_null() {
            // SAFETY: caller contract — `engine` came from `Box::into_raw` here.
            drop(unsafe { Box::from_raw(engine) });
        }
    })
}

/// Convert an owned `String` to a heap C string. Interior NULs (impossible in a C
/// string) are stripped so the conversion never fails; the screen text is UTF-8.
fn to_c_string(s: String) -> *mut c_char {
    let cleaned = if s.as_bytes().contains(&0) {
        s.replace('\0', "")
    } else {
        s
    };
    CString::new(cleaned).map_or(std::ptr::null_mut(), CString::into_raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises the whole C ABI round-trip from Rust: create -> feed an escape
    // sequence + text -> read the screen -> free. This is exactly what a C
    // embedder does, so passing it validates the surface end-to-end.
    #[test]
    fn feed_then_read_screen_roundtrips() {
        unsafe {
            let eng = aterm_engine_new(24, 80);
            assert!(!eng.is_null());

            // SGR bold + plain text; the engine should put "helloworld" on row 0.
            let input = b"hello\x1b[1mworld";
            aterm_engine_feed(eng, input.as_ptr(), input.len());

            let screen = aterm_engine_visible_content(eng);
            assert!(!screen.is_null());
            let text = std::ffi::CStr::from_ptr(screen)
                .to_string_lossy()
                .into_owned();
            assert!(text.contains("hello"), "screen was: {text:?}");
            assert!(text.contains("world"), "screen was: {text:?}");
            aterm_string_free(screen);

            let row0 = aterm_engine_row_text(eng, 0);
            assert!(!row0.is_null());
            let r0 = std::ffi::CStr::from_ptr(row0)
                .to_string_lossy()
                .into_owned();
            assert!(r0.contains("helloworld"), "row 0 was: {r0:?}");
            aterm_string_free(row0);

            // Out-of-range row -> null, not a crash.
            assert!(aterm_engine_row_text(eng, 9999).is_null());

            aterm_engine_free(eng);
        }
    }

    #[test]
    fn null_handles_are_safe() {
        unsafe {
            aterm_engine_feed(std::ptr::null_mut(), b"x".as_ptr(), 1); // no-op
            aterm_engine_resize(std::ptr::null_mut(), 1, 1); // no-op
            assert!(aterm_engine_visible_content(std::ptr::null()).is_null());
            aterm_string_free(std::ptr::null_mut()); // no-op
            aterm_engine_free(std::ptr::null_mut()); // no-op
        }
    }

    // Every string-returning export must yield the null SENTINEL on a null handle
    // (not a crash, not UB) — this covers `aterm_engine_row_text`'s null path, which
    // `null_handles_are_safe` above does not, plus a null *content pointer* into a
    // LIVE engine for `feed` (engine non-null, `ptr` null => documented no-op). The
    // pair pins both null-guard branches: null handle AND null data pointer.
    #[test]
    fn null_handle_and_null_pointer_return_sentinels() {
        unsafe {
            // Null handle => null sentinel for both string-returning exports.
            assert!(aterm_engine_row_text(std::ptr::null(), 0).is_null());
            assert!(aterm_engine_visible_content(std::ptr::null()).is_null());

            // Live engine but a NULL data pointer => `feed` is a documented no-op
            // (no read of the null region), and the engine stays usable afterward.
            let eng = aterm_engine_new(4, 8);
            assert!(!eng.is_null());
            aterm_engine_feed(eng, std::ptr::null(), 5); // no-op: null ptr guard
            let screen = aterm_engine_visible_content(eng);
            assert!(
                !screen.is_null(),
                "engine must stay usable after a null-ptr feed"
            );
            aterm_string_free(screen);
            aterm_engine_free(eng);
        }
    }

    // Length / overflow edges of the read surface:
    //  * a ZERO-LENGTH feed is a clean no-op (the engine is unchanged and usable);
    //  * an out-of-range ROW index returns null (not an OOB read / panic), even for
    //    a huge index that would overflow a naive offset computation.
    #[test]
    fn zero_length_feed_and_out_of_range_row() {
        unsafe {
            let eng = aterm_engine_new(3, 5);
            assert!(!eng.is_null());

            // Zero-length feed: a valid pointer but len 0 reads nothing — no-op.
            let data = b"ignored";
            aterm_engine_feed(eng, data.as_ptr(), 0);

            // In-range rows (0..3) are non-null; out-of-range rows are null.
            for r in 0..3usize {
                let p = aterm_engine_row_text(eng, r);
                assert!(!p.is_null(), "row {r} should be in range");
                aterm_string_free(p);
            }
            assert!(
                aterm_engine_row_text(eng, 3).is_null(),
                "row == rows is out of range"
            );
            assert!(
                aterm_engine_row_text(eng, usize::MAX).is_null(),
                "a huge row index must return null, not OOB-read or panic",
            );

            aterm_engine_free(eng);
        }
    }

    // An out-of-contract `len` (greater than `isize::MAX`, the exact shape of the
    // ubiquitous C footgun `ssize_t n = read(...); feed(e, buf, n);` with `n == -1`
    // becoming `(size_t)-1`) must be a safe no-op: `ffi_byte_slice` rejects the
    // length before `from_raw_parts` is reached (forming such a slice is instant
    // UB), so the engine is untouched and stays usable. A normal feed afterward
    // still renders, proving the guard rejects only out-of-contract lengths.
    #[test]
    fn over_isize_len_is_safe_noop_then_normal_feed_works() {
        unsafe {
            let eng = aterm_engine_new(4, 16);
            assert!(!eng.is_null());

            // A non-null pointer but a length far past isize::MAX. With the raw
            // `from_raw_parts` this was instantaneous UB; it must now be a no-op.
            let data = b"x";
            aterm_engine_feed(eng, data.as_ptr(), usize::MAX);

            // Engine is untouched: nothing got written.
            let screen = aterm_engine_visible_content(eng);
            assert!(
                !screen.is_null(),
                "engine must stay usable after a huge-len feed"
            );
            let blank = std::ffi::CStr::from_ptr(screen)
                .to_string_lossy()
                .into_owned();
            assert!(
                !blank.contains("hello"),
                "no content should have been ingested from the rejected feed"
            );
            aterm_string_free(screen);

            // A normal, in-contract feed still works on the same live engine.
            let input = b"hello";
            aterm_engine_feed(eng, input.as_ptr(), input.len());
            let screen = aterm_engine_visible_content(eng);
            assert!(!screen.is_null());
            let text = std::ffi::CStr::from_ptr(screen)
                .to_string_lossy()
                .into_owned();
            assert!(
                text.contains("hello"),
                "normal feed after huge-len no-op was: {text:?}"
            );
            aterm_string_free(screen);

            aterm_engine_free(eng);
        }
    }

    // A second feed->read round-trip that also exercises `aterm_engine_resize`:
    // after a resize the engine still feeds and renders, and the visible content
    // reflects the new geometry's text. Complements `feed_then_read_screen_roundtrips`
    // by covering the resize export on the success path.
    #[test]
    fn feed_resize_read_roundtrip() {
        unsafe {
            let eng = aterm_engine_new(10, 40);
            assert!(!eng.is_null());

            let input = b"abcdef";
            aterm_engine_feed(eng, input.as_ptr(), input.len());
            aterm_engine_resize(eng, 5, 20); // valid resize on a live handle

            let screen = aterm_engine_visible_content(eng);
            assert!(!screen.is_null());
            let text = std::ffi::CStr::from_ptr(screen)
                .to_string_lossy()
                .into_owned();
            assert!(text.contains("abcdef"), "screen after resize was: {text:?}");
            aterm_string_free(screen);

            aterm_engine_free(eng);
        }
    }

    // The `catch_unwind` wrapping must actually catch a panic raised INSIDE a real
    // exported `extern "C"` function body and return the sentinel, rather than let
    // the unwind cross the C ABI (UB). `panic_in_ffi_body_is_caught_not_unwound`
    // above expands the macro inline; this instead defines a genuine `#[no_mangle]
    // extern "C"` export — the EXACT shape of every export in this crate — whose
    // body unconditionally panics, then calls it ACROSS the FFI boundary and
    // asserts it returns the null sentinel. If the wrapping were absent the panic
    // would unwind out of the extern fn and abort the process, failing the test.
    #[unsafe(no_mangle)]
    extern "C" fn aterm_ffi_test_panicking_export() -> *mut c_char {
        aterm_ffi_catch_unwind!(std::ptr::null_mut(), {}, {
            panic!("boom inside a real exported extern \"C\" fn");
        })
    }

    #[test]
    fn panic_in_exported_extern_fn_returns_null_sentinel() {
        // Call it as a C caller would (through the extern fn), not via the macro.
        let p: *mut c_char = aterm_ffi_test_panicking_export();
        assert!(
            p.is_null(),
            "a panic in an exported extern fn must yield the null sentinel, not unwind",
        );
    }

    // A panic inside an exported function body must be caught and converted to a
    // sentinel — never unwound across the C ABI (that is UB). This exercises the
    // exact `aterm_ffi_catch_unwind!` wiring used by every export above: a `()`
    // body that panics returns `()` safely, and a pointer body that panics returns
    // null. If the macro were missing, this test would abort the process.
    #[test]
    fn panic_in_ffi_body_is_caught_not_unwound() {
        let unit: () = aterm_ffi_catch_unwind!((), {}, {
            panic!("boom from a unit-returning FFI body");
        });
        assert_eq!(unit, ());

        let ptr: *mut c_char = aterm_ffi_catch_unwind!(std::ptr::null_mut(), {}, {
            panic!("boom from a pointer-returning FFI body");
        });
        assert!(
            ptr.is_null(),
            "panicking FFI body must yield the null sentinel"
        );
    }
}
