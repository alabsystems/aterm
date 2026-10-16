// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The static-layout suite: the aterm-objc integration files that only read the runtime (class
//! and method tables, encodings, ABI layout) or the source tree, as modules of one test binary.
//! A file that owns process-global state a sibling would see (a swizzle, a panic hook, a global
//! allocator, the protocol table, a self re-exec) is its own `[[test]]` in `Cargo.toml`, with
//! the reason on the line above it; `main_thread_bound_drive` has no libtest harness.
//!
//! Sharing one process is sound because nothing here mutates state another module reads: the
//! two declared classes (`ATermObjcProbe`, `ATermW3StretProbe`) have unique names, framework
//! loads are idempotent `OnceLock`s, `conformance_wake_timer` arms timers on its own thread's
//! run loop, and no file re-execs itself, sets a hook, or touches the environment.
//! `encode_offsets` walks every class, the two probes included.
//!
//! The `tests/support/` files are mounted once here and reached as `crate::…`; `winit_seam`
//! keeps its own mounts. With `autotests = false`, a `tests/*.rs` file that is neither a `mod`
//! line here nor its own `[[test]]` never compiles.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-objc --test objc_static_suite -- <file>::`.

#[cfg(target_os = "macos")]
#[path = "support/sdk_consts.rs"]
mod sdk_consts;

mod abi;
mod blocks;
mod cf_cg_sdk;
mod cf_runtime;
mod conformance_wake_timer;
mod declared_class;
mod encode_offsets;
mod gui_appkit_constants;
mod main_thread_bound;
mod runtime;
mod stret_declared;
mod winit_seam_constants;
