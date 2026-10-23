// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libc` — aterm's first-party replacement for the crates.io `libc` package.
//!
//! Named `libc` so `[patch.crates-io]` redirects every workspace and third-party
//! consumer here. Each (OS, arch) cell declares the platform ABI and is generated
//! by `aterm-libc-oracle regen` (from `rustdoc` JSON and const-eval layout probes)
//! and verified against the pinned registry crate by `libc-oracle`; never hand-edit
//! the cells.

#![no_std]
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

pub use core::ffi::{
    c_char, c_double, c_float, c_int, c_long, c_longlong, c_schar, c_short, c_uchar, c_uint,
    c_ulong, c_ulonglong, c_ushort, c_void,
};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod darwin_aarch64;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use crate::darwin_aarch64::*;

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod darwin_x86_64;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub use crate::darwin_x86_64::*;

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
mod linux_gnu_x86_64;
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
pub use crate::linux_gnu_x86_64::*;

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
mod linux_gnu_aarch64;
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
pub use crate::linux_gnu_aarch64::*;

// Target gate: Unix triples use their generated cells; Windows and wasm32 export only `core::ffi`.
#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"),
    all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"),
    all(target_os = "windows", target_env = "msvc", target_arch = "aarch64"),
    all(target_os = "windows", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "unknown", target_arch = "wasm32")
)))]
compile_error!("aterm-libc has no generated ABI cell for this target");
