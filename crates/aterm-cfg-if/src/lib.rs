// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `cfg-if` — aterm's first-party `cfg_if!` macro.

#![no_std]
#![warn(missing_docs)]

/// Emit the items of the first arm whose `#[cfg]` predicate holds.
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
