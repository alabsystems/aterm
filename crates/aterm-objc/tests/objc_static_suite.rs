// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The static-layout suite: the aterm-objc integration files that only read the runtime (class
//! and method tables, encodings, ABI layout) or hold a declaration to the SDK headers, as modules
//! of one test binary.
//!
//! Each file stays at its own path (every `tests/<file>.rs:<line>` citation
//! still resolves) and keeps its own `#[test]`s. What stays OUT, each as its own
//! `[[test]]` in `Cargo.toml` with the reason on the line above it, is every
//! file that owns process-global state a sibling would see: `swizzle` (method
//! implementation swaps on shared classes), `weak` (a `#[global_allocator]`),
//! `protocol_registration` (the runtime's one protocol table), and the process
//! suite (`objc_process_suite`: the tests that re-run the binary to watch a
//! child die, and the containment sink).
//! `main_thread_bound_drive` has no libtest harness and stays its own target.
//!
//! Sharing one process here is sound because nothing below mutates process
//! state another module reads: every class declared here has a unique name,
//! the framework loads are idempotent `OnceLock`s, `conformance_wake_timer`
//! arms timers on its own thread's run loop, and no file re-execs itself, sets
//! a hook, or touches the environment. `encode_offsets` walks every class in
//! the process, so it also sees the probe classes — whose encodings must
//! satisfy the same "strip deletes offsets only" law as AppKit's.
//!
//! The census support files under `tests/support/` are mounted ONCE here and
//! reached as `crate::…`, so one binary compiles each once.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-objc --test objc_static_suite -- <file>::`.

#[cfg(target_os = "macos")]
#[path = "support/sdk_consts.rs"]
mod sdk_consts;
#[cfg(target_os = "macos")]
#[path = "support/sdk_decls.rs"]
mod sdk_decls;

mod abi;
mod blocks;
mod cf_runtime;
mod cf_sdk;
mod conformance_wake_timer;
mod declared_abi;
mod declared_class;
mod declared_lifetimes;
mod encode_offsets;
mod gui_appkit_constants;
mod main_thread_bound;
mod runtime;
mod stret_declared;
mod winit_seam_constants;
