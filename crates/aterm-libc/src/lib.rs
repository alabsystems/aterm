// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libc` — aterm's first-party replacement for the crates.io `libc` package.
//!
//! The package is named `libc` (the directory is `crates/aterm-libc`) so
//! `[patch.crates-io]` redirects every consumer, ours and third-party, here.
//!
//! It DECLARES the platform ABI — constants, `extern "C"` signatures, struct
//! layouts, type aliases — per (OS, arch) cell; the symbols still resolve to the
//! system C library at link time.
//!
//! ## The cells are generated: never hand-edit them
//!
//! A wrong ABI number neither fails to compile nor panics; it passes the wrong
//! flag to a syscall or hands the kernel an undersized out-parameter. So each
//! cell is generated per triple (`aterm-libc-oracle regen`) from:
//!
//! * `rustdoc --output-format json` of the reference libc built for that
//!   triple: constant values, signatures, `link_name`s, field names and types,
//!   enum discriminants;
//! * const-eval probes compiled for that triple (an array-length mismatch
//!   prints the number): sizes, alignments, public-field offsets and the bytes
//!   of struct-valued initializers — measured, even on cells this machine
//!   cannot run.
//!
//! The conformance oracle then asserts each cell against the pinned registry
//! crate: constant values, `size_of`, `align_of`, `offset_of!` and type of every
//! public field, whole-signature function-pointer coercions, two-way alias
//! identity, and the exact public inherent method set. The hand-written C-macro
//! bodies and `siginfo_t` accessors have differential behavior coverage: status
//! helpers over `-1..=0xFFFF` plus signed/high-bit extremes, pointer-writing
//! helpers on the native cell.
//!
//! Shim and oracle are emitted from ONE name set: the harvested items
//! (`libc-oracle/union.json`) plus the transitive closure of the types they
//! reference. Emitting the oracle from the union alone once left closure-only
//! types (`cmsghdr`, `vm_statistics64`, …) declared but unchecked. The
//! regeneration fails if any DECLARED name or public field ends without an
//! assertion.
//!
//! ## Divergences from the crates.io package
//!
//! * **Only the cells aterm builds.** Any other target hits `compile_error!`
//!   rather than inheriting another cell's values.
//! * **Private fields are renamed** `__pad`/`__opaque`, at their measured
//!   extent. Size, alignment and public offsets match; `Debug` output does not.
//! * **`extra_traits` derives** `PartialEq`/`Eq`/`Hash` where libc hand-writes
//!   some.
//! * **`vm_statistics64` is upstream's Mach rev1** (24 fields, 152 bytes), not
//!   the SDK's rev3 (36, 248), matching the replaced crate byte-for-byte. The
//!   reasoning is on the struct in the Darwin cells (from
//!   `crates/aterm-libc-oracle/src/regen/handwritten.rs`).
//! * `std`, `align`, `const-extern-fn`, `use_std` and `rustc-dep-of-std` are
//!   accepted and ignored; nothing in aterm's graph varies on them.

#![no_std]
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

// `core::ffi`'s C primitive aliases are the same on every cell, so they are
// re-exported unconditionally, including on cells with no other declarations.
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

// THE TARGET GATE. The Windows and wasm32 rows have no `mod`: those targets
// have no POSIX libc, every consumer's `libc::` use is `cfg`-gated away there,
// and the `core::ffi` aliases above are all they need (`libc-oracle`'s
// `cell_zero` checks that much). A row without a `mod` claims the target needs
// no ABI declarations, which is false for any Unix triple: a new Unix triple
// brings a generated cell.
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
