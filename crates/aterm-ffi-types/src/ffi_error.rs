// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared FFI error logging macro.
//!
//! Provides [`aterm_ffi_error!`] — a centralized error-logging macro for FFI
//! boundaries. Each domain crate defines a thin local alias that binds its own
//! log prefix (e.g. `"[aterm-editor-ffi]"`), keeping call sites unchanged.
//!
//! Follows the same `#[macro_export]` pattern as [`aterm_ffi_catch_panic!`] in
//! `ffi_panic.rs`.

/// Log an FFI error with a crate-specific prefix, gated on `ffi-logging`.
///
/// Returns the error value unchanged. When the `ffi-logging` feature is
/// enabled in the calling crate, logs the message via `aterm_log::error!`
/// before returning. When disabled, returns the error unchanged without
/// emitting log output while still evaluating formatting inputs.
///
/// # Arguments
///
/// * `$log_prefix` — logging prefix literal (e.g. `"[aterm-editor-ffi]"`)
/// * `$err` — error value to return
/// * `$msg` — error message literal
///
/// # Examples
///
/// ```text
/// // In a domain crate's FFI module:
/// macro_rules! ffi_error {
///     ($err:expr, $msg:literal) => {
///         aterm_ffi_error!("[aterm-editor-ffi]", $err, $msg)
///     };
///     ($err:expr, $fmt:literal, $($arg:tt)*) => {
///         aterm_ffi_error!("[aterm-editor-ffi]", $err, $fmt, $($arg)*)
///     };
/// }
/// ```
#[macro_export]
macro_rules! aterm_ffi_error {
    // Simple form: literal message
    ($log_prefix:literal, $err:expr_2021, $msg:literal) => {{
        #[cfg(feature = "ffi-logging")]
        aterm_log::error!("{} {}", $log_prefix, $msg);
        $err
    }};
    // Format form: message with arguments
    ($log_prefix:literal, $err:expr_2021, $fmt:literal, $($arg:tt)*) => {{
        #[cfg(feature = "ffi-logging")]
        aterm_log::error!(concat!($log_prefix, " ", $fmt), $($arg)*);
        #[cfg(not(feature = "ffi-logging"))]
        let _ = format_args!($fmt, $($arg)*);
        $err
    }};
    // Expression form: message from concat!() or other compile-time expression.
    // Falls through from the literal arm when the message is not a bare literal
    // token (e.g. `concat!($fn_name, ": null scene")`).
    ($log_prefix:literal, $err:expr_2021, $msg:expr_2021) => {{
        #[cfg(feature = "ffi-logging")]
        aterm_log::error!("{} {}", $log_prefix, $msg);
        #[cfg(not(feature = "ffi-logging"))]
        let _ = format_args!("{}", $msg);
        $err
    }};
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "ffi-logging"))]
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(not(feature = "ffi-logging"))]
    fn observe_format_arg(counter: &AtomicUsize) -> u8 {
        counter.fetch_add(1, Ordering::SeqCst);
        99
    }

    #[cfg(not(feature = "ffi-logging"))]
    fn observe_expr_message(counter: &AtomicUsize) -> String {
        counter.fetch_add(1, Ordering::SeqCst);
        "dynamic error".to_owned()
    }

    #[test]
    fn ffi_error_returns_error_value() {
        let val: i32 = aterm_ffi_error!("[test]", -42, "something failed");
        assert_eq!(val, -42);
    }

    #[test]
    fn ffi_error_format_returns_error_value() {
        let _code = 99u8;
        let val: i32 = aterm_ffi_error!("[test]", -1, "error code {}", _code);
        assert_eq!(val, -1);
    }

    #[test]
    fn ffi_error_expr_returns_error_value() {
        let val: i32 = aterm_ffi_error!("[test]", -7, String::from("dynamic error"));
        assert_eq!(val, -7);
    }

    #[cfg(not(feature = "ffi-logging"))]
    #[test]
    fn ffi_error_format_evaluates_arguments_without_logging() {
        let counter = AtomicUsize::new(0);
        let val: i32 =
            aterm_ffi_error!("[test]", -1, "error code {}", observe_format_arg(&counter));
        assert_eq!(val, -1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[cfg(not(feature = "ffi-logging"))]
    #[test]
    fn ffi_error_expr_evaluates_message_without_logging() {
        let counter = AtomicUsize::new(0);
        let val: i32 = aterm_ffi_error!("[test]", -7, observe_expr_message(&counter));
        assert_eq!(val, -7);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
