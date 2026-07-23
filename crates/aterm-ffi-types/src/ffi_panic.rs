// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared panic-catching helpers for FFI boundary macros.

/// Extract a human-readable message from a borrowed panic payload:
/// the `&str`/`String` payload when present, a placeholder otherwise.
///
/// Trust L0: `#[inline(never)]`, `unsafe`-free helper. Neither std's
/// `AsRef::as_ref`/`downcast_ref` instances (not present in the TrustIr
/// module) nor a built-in `Box` deref (lowers to a `NonNull` to raw-pointer
/// cast the verifier cannot lower) may appear in the MIR of the FFI wrappers
/// that expand `aterm_ffi_catch_unwind!`. This helper is TrustIr-present
/// (callable opaquely) and contains no `unsafe`, so the unlowerable std
/// calls inside it are memory-safe warning-grade coverage, not a gate
/// failure.
#[doc(hidden)]
#[inline(never)]
pub fn panic_payload_msg(payload: &Box<dyn core::any::Any + Send>) -> &str {
    let payload_ref: &(dyn core::any::Any + Send) = payload.as_ref();
    payload_ref
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload_ref.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("<non-string panic payload>")
}

/// Catch panics at an FFI boundary and return a default value on panic.
///
/// This macro centralizes the `catch_unwind` implementation so domain crates can
/// customize logging/prefix behavior while sharing one safety primitive.
///
/// # Arguments
///
/// - `$default`: value returned when a panic is caught.
/// - `$on_panic`: expression executed when a panic is caught (for logging).
/// - `$body`: FFI function body to execute.
#[macro_export]
macro_rules! aterm_ffi_catch_unwind {
    ($default:expr_2021, $on_panic:expr_2021, $body:expr_2021) => {{
        #[cfg(kani)]
        {
            // Kani cannot model `catch_unwind` (kani#267); verify inner logic directly.
            $body
        }
        #[cfg(not(kani))]
        {
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| $body)) {
                Ok(result) => result,
                Err(_panic) => {
                    // Extract panic message for diagnostics (#5892, F11-2 #7941).
                    //
                    // Never silently mask an FFI panic: log to stderr *and* to
                    // the structured log sink so observability pipelines see
                    // it even when the `ffi-logging` feature is off.
                    //
                    // Trust L0: extraction lives in `$crate::panic_payload_msg`
                    // (called on `&_panic` — no `Box` deref here) so the
                    // deref/downcast MIR the Trust full verifier cannot lower
                    // stays out of the FFI wrapper this macro expands into, and
                    // the logging itself runs under a nested `catch_unwind`:
                    // formatted `eprintln!` in the wrapper's own MIR turns the
                    // wrapper into a native-verification root the verifier can
                    // never fully lower (std io + `fmt::Arguments::new`), while
                    // `catch_unwind`-guarded closures verify like the `$body`
                    // closure above. This also hardens the boundary: a panic
                    // raised by the logging itself (e.g. stderr write failure)
                    // can no longer unwind into the foreign caller — the
                    // fallback error code is still returned.
                    let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                        let _msg_str = $crate::panic_payload_msg(&_panic);
                        eprintln!("[aterm-ffi] panic caught: {_msg_str}");
                        $crate::aterm_log::error!(
                            "[aterm-ffi] FFI call panicked — returning error code: {}",
                            _msg_str
                        );
                    }));
                    $on_panic;
                    $default
                }
            }
        }
    }};
}

/// Catch panics at an FFI boundary with crate-specific logging prefix support.
///
/// This macro wraps `aterm_ffi_catch_unwind!` and standardizes panic logging
/// format while letting each FFI domain specify its own log prefix.
///
/// # Arguments
///
/// - `$log_prefix`: logging prefix (for example `"[aterm-editor-ffi]"`).
/// - `$default`: value returned when a panic is caught.
/// - `$fn_name`: function name used in panic logs.
/// - `$body`: FFI function body to execute.
#[macro_export]
macro_rules! aterm_ffi_catch_panic {
    ($log_prefix:literal, $default:expr_2021, $fn_name:literal, $body:expr_2021) => {
        $crate::aterm_ffi_catch_unwind!(
            $default,
            {
                // F11-2 (#7941): log the prefix+fn_name even without
                // the `ffi-logging` feature so panic attribution is
                // never silently dropped.
                $crate::aterm_log::error!("{} {}: panic caught", $log_prefix, $fn_name);
            },
            $body
        )
    };
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn shared_macro_returns_body_value_on_success() {
        let ran = AtomicBool::new(false);
        let value: i32 = aterm_ffi_catch_unwind!(-1, { ran.store(true, Ordering::Relaxed) }, { 7 });
        assert_eq!(value, 7);
        assert!(
            !ran.load(Ordering::Relaxed),
            "on_panic should not run on success"
        );
    }

    #[test]
    fn shared_macro_returns_default_on_panic() {
        let ran = AtomicBool::new(false);
        let value: i32 = aterm_ffi_catch_unwind!(-1, { ran.store(true, Ordering::Relaxed) }, {
            panic!("boom");
        });
        assert_eq!(value, -1);
        assert!(ran.load(Ordering::Relaxed), "on_panic should run on panic");
    }

    #[test]
    fn panic_macro_uses_default_on_panic() {
        let value: i32 = aterm_ffi_catch_panic!("[aterm-test-ffi]", -1, "test_fn", {
            panic!("boom");
        });
        assert_eq!(value, -1);
    }

    #[test]
    fn panic_macro_returns_body_value() {
        let value: i32 = aterm_ffi_catch_panic!("[aterm-test-ffi]", -1, "test_fn", { 11 });
        assert_eq!(value, 11);
    }
}
