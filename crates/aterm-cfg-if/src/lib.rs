// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `cfg-if` — aterm's first-party `cfg_if!` macro.

#![no_std]
#![warn(missing_docs)]

/// Emit the items of the first arm whose `#[cfg]` predicate holds.
///
/// ```
/// cfg_if::cfg_if! {
///     if #[cfg(unix)] {
///         fn platform() -> &'static str { "unix" }
///     } else if #[cfg(windows)] {
///         fn platform() -> &'static str { "windows" }
///     } else {
///         fn platform() -> &'static str { "other" }
///     }
/// }
/// # fn main() {}
/// ```
///
/// The `else` is optional; when no predicate holds the macro expands to
/// nothing, so it can sit in statement position.
///
/// ```
/// fn f() -> Result<(), ()> {
///     cfg_if::cfg_if! {
///         if #[cfg(any())] {
///             return Err(());
///         }
///     };
///     Ok(())
/// }
/// assert!(f().is_ok());
/// ```
///
/// Arms that are not selected are never expanded, so `compile_error!` works as
/// an unsupported-platform guard:
///
/// ```
/// cfg_if::cfg_if! {
///     if #[cfg(all())] {
///         fn supported() -> bool { true }
///     } else {
///         compile_error!("unsupported platform");
///     }
/// }
/// assert!(supported());
/// ```
///
/// Control: the same `compile_error!` in the selected arm fails the build.
///
/// ```compile_fail
/// cfg_if::cfg_if! {
///     if #[cfg(all())] {
///         compile_error!("this arm is selected, so this must fire");
///     }
/// }
/// ```
#[macro_export]
macro_rules! cfg_if {
    (
        if #[cfg( $($i_meta:tt)+ )] { $( $i_tokens:tt )* }
        $(
            else if #[cfg( $($ei_meta:tt)+ )] { $( $ei_tokens:tt )* }
        )*
        $(
            else { $( $e_tokens:tt )* }
        )?
    ) => {
        $crate::cfg_if! {
            @__arms () ;
            (( $($i_meta)+ ) ( $( $i_tokens )* )),
            $(
                (( $($ei_meta)+ ) ( $( $ei_tokens )* )),
            )*
            $(
                (() ( $( $e_tokens )* )),
            )?
        }
    };

    (@__arms ( $( ($($_seen:tt)*) , )* ) ; ) => {};

    (
        @__arms ( $( ($($no:tt)+) , )* ) ;
        (( $( $($yes:tt)+ )? ) ( $( $tokens:tt )* )),
        $( $rest:tt , )*
    ) => {
        #[cfg(all(
            $( $($yes)+ , )?
            not(any( $( $($no)+ ),* ))
        ))]
        $crate::cfg_if! { @__group $( $tokens )* }

        $crate::cfg_if! {
            @__arms ( $( ($($no)+) , )* $( ($($yes)+) , )? ) ;
            $( $rest , )*
        }
    };

    (@__group $( $tokens:tt )* ) => {
        $( $tokens )*
    };
}
