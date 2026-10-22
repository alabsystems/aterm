// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libc` — aterm's first-party replacement for the crates.io `libc` package.
//!
//! Named `libc` (in `crates/aterm-libc`) so `[patch.crates-io]` redirects every
//! consumer, ours and third-party, here. It declares the platform ABI —
//! constants, `extern "C"` signatures, struct layouts, type aliases — per (OS,
//! arch) cell; the symbols resolve to the system C library at link time.
//!
//! ## The cells are generated: never hand-edit them
//!
//! A wrong ABI number neither fails to compile nor panics; it passes the wrong
//! flag to a syscall or hands the kernel an undersized out-parameter. So each
//! cell is generated per triple (`libc-oracle/gen/regen.sh`) from:
//!
//! * `rustdoc --output-format json` of the reference libc built for that
//!   triple: constant values, signatures, `link_name`s, field names and types,
//!   enum discriminants;
//! * const-eval probes compiled for that triple (an array-length mismatch
//!   prints the number): sizes, alignments, public-field offsets and the bytes
//!   of struct-valued initializers, measured even on cells this machine cannot
//!   run.
//!
//! The conformance oracle asserts each cell against the pinned registry crate:
//! constant values, every type's size and alignment, every public field's offset
//! and type, whole function signatures, alias identity and the exact public
//! inherent method set. The hand-written C-macro bodies and `siginfo_t`
//! accessors are tested differentially against it.
//!
//! Shim and oracle are emitted from one name set: the harvested items
//! (`libc-oracle/gen/union.json`) plus the transitive closure of the types they
//! reference (`libc-oracle/gen/closure.py`). `build.py` fails if any emitted
//! name or public field has no assertion.
//!
//! ## Divergences from the crates.io package
//!
//! * Only the cells aterm builds: a target the gate below does not list hits
//!   `compile_error!` rather than inheriting another cell's values.
//! * Private fields are renamed `__pad`/`__opaque` at their measured extent.
//!   Size, alignment and public offsets match; `Debug` output does not.
//! * `extra_traits` derives `PartialEq`/`Eq`/`Hash` where libc hand-writes some.
//! * `vm_statistics64` is upstream's Mach rev1 (24 fields, 152 bytes), not the
//!   SDK's rev3 (36, 248); the reasoning is on the struct in the Darwin cells.
//! * `std`, `align`, `const-extern-fn`, `use_std` and `rustc-dep-of-std` are
//!   accepted and ignored.

#![no_std]
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

// `core::ffi` defines the C primitive aliases correctly for every target, so they are
// re-exported unconditionally, including on the Windows and wasm32 rows that have no cell.
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

// The target gate. Windows and wasm32 rows have no `mod`: those targets have no
// POSIX libc, every consumer's `libc::` use is `cfg`-gated away there, and the
// `core::ffi` aliases above are all they need (`libc-oracle`'s `cell_zero`
// checks that). A new Unix triple needs a generated cell, not a bare row.
//
// Rows are per (os, env, arch): each Windows arch and env is listed on its own.
// A missing row kills a lane before any aterm code is type-checked: `verify
// cells` compiles every forge cell through this gate.
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
