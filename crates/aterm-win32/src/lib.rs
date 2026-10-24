// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The `link!` macro is derived from windows-link 0.2.1 src/lib.rs and modified by the aterm
// project in 2026 (`unsafe extern`, as edition 2024 requires). See ../LICENSE-MIT and
// ../LICENSE-APACHE for upstream licenses.

//! Win32, COM, and DXGI declarations for aterm's Windows targets ([`com`], [`dxgi`]).

#![no_std]
#![allow(non_snake_case, non_upper_case_globals, non_camel_case_types)]

/// Declares one imported function.
#[cfg(all(windows, target_arch = "x86"))]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim", import_name_type = "undecorated")]
        unsafe extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Declares one imported function.
#[cfg(all(windows, not(target_arch = "x86")))]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim")]
        unsafe extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Declares one imported function with no link attribute.
#[cfg(not(windows))]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        unsafe extern $abi {
            pub fn $($function)*;
        }
    )
}

pub mod Win32;
pub mod com;
pub mod core;
pub mod dxgi;
