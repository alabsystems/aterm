// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libc` — aterm's first-party replacement for the crates.io `libc` package.
//!
//! This crate is published into the build under the package name `libc` (the
//! directory is `crates/aterm-libc`) so `[patch.crates-io]` can redirect every
//! consumer — ours and the third-party ones — at this code.
//!
//! ## What this is
//!
//! A set of DECLARATIONS of the platform ABI: constants, `extern "C"` function
//! signatures, struct layouts and type aliases, declared per (OS, arch) cell.
//! It is not a reimplementation of libc; the symbols still resolve to the
//! system's C library at link time.
//!
//! ## How the values got here, and why you must not hand-edit them
//!
//! Every number in the cell modules is a fact about one platform's ABI, and a
//! wrong one does not fail to compile and does not panic — it passes the wrong
//! flag to a syscall, or hands the kernel an undersized out-parameter. So none
//! of them were typed. The cell modules are GENERATED from two mechanical
//! sources, per triple:
//!
//! * constant values, function signatures, `link_name`s, struct field names
//!   and types, and enum discriminants come from `cargo rustdoc
//!   --output-format json` run against the reference libc built FOR THAT
//!   TRIPLE;
//! * struct sizes, alignments, public-field offsets and the bytes of the
//!   struct-valued initializers come from const-eval probes compiled for that
//!   triple (a deliberate array-length mismatch prints the number), so they are
//!   measured rather than derived — including on cells this machine cannot run.
//!
//! and every one of them is then asserted against the exact pinned registry
//! reference crate by the conformance oracle (the local shim alone is renamed),
//! per cell: constant values,
//! `size_of`, `align_of`, `offset_of!` for every public field, the type of every
//! public field, whole-signature function-pointer coercions, two-way alias
//! identity (including nominal alias referents), and the exact public inherent
//! method set and signatures. Every hand-written C-macro body and `siginfo_t`
//! accessor also has differential behavior coverage; status helpers are swept
//! over `-1..=0xFFFF` plus signed/high-bit extremes, while pointer-writing
//! helpers and accessors run on the native cell. Regenerate rather than edit.
//!
//! ## The oracle checks exactly what this crate declares
//!
//! Both sides are emitted from ONE set of names: the harvested item list
//! (`libc-oracle/gen/union.json`) plus the transitive closure of the types those
//! items reference, computed in `libc-oracle/gen/closure.py` and imported by the
//! emitter and the oracle builder alike. That sharing is load-bearing, and it is
//! the answer to a real defect: for one release the shim was emitted from the
//! closure while the oracle was emitted from the union alone, so the types that
//! entered ONLY through the closure -- `cmsghdr`, `tls_crypto_info`, `timezone`,
//! `__u16` here on linux; `vm_statistics64`, `natural_t`, `integer_t` on the
//! Darwin cells -- were declared and never checked. Twenty-nine public fields
//! had no layout assertion behind them, and reversing `cmsghdr`'s field order
//! (the SCM_RIGHTS control-message header) left the oracle green.
//!
//! `build.py` also fails the build if any declared name or public field reaches
//! the end without an assertion recorded against it. That tally is taken from
//! what the generator EMITTED, not from the item list it started with: the old
//! coverage number was computed from the same union as the assertions, so it
//! shared their blind spot and reported zero uncovered forever.
//!
//! ## DIVERGENCES from the crates.io package, stated
//!
//! * **Only the cells aterm builds are declared.** An unsupported target is
//!   rejected explicitly with `compile_error!` rather than inheriting a wrong
//!   value or silently passing through an unrelated target cell.
//! * **Private fields are named differently.** Where libc keeps a struct's
//!   internals private, this crate reproduces their measured extent as
//!   `__pad`/`__opaque` members. Size, alignment and every public field offset
//!   match exactly; `Debug` output does not.
//! * **`extra_traits` derives rather than hand-writes.** libc implements
//!   `PartialEq`/`Eq`/`Hash` by hand for some types; here they are derived.
//! * **`vm_statistics64` is upstream's Mach rev1, not the SDK's rev3.** 24
//!   fields and 152 bytes here against the SDK's 36 and 248. The divergence is
//!   INHERITED -- this crate matches the `libc` it replaces byte-for-byte -- and
//!   it is kept on purpose: nothing in aterm's graph calls `host_statistics64`,
//!   so nothing can observe it, and for a drop-in replacement differing from the
//!   crate replaced is the worse property. The reasoning, the evidence and what
//!   to do if a caller ever appears are on the struct itself in the Darwin cell
//!   modules (and in `libc-oracle/gen/emit.py`, which is what puts them there).
//! * The feature table is upstream's, and `std`, `align`, `const-extern-fn`,
//!   `use_std` and `rustc-dep-of-std` are accepted and ignored: nothing in
//!   aterm's graph varies on them (`cargo tree -e features -i libc` resolves to
//!   `default,extra_traits,std` on the unix cells and `default,std` elsewhere).

#![no_std]
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

// The C primitive aliases are `core`'s own definitions on every cell, so they
// are re-exported unconditionally: there is no per-platform fact to get wrong,
// and a caller that names one on a cell with no other declarations still gets
// the same type `std` would hand it.
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

#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"),
    all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"),
    all(target_os = "unknown", target_arch = "wasm32")
)))]
compile_error!("aterm-libc has no generated ABI cell for this target");
