// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATIC-LAYOUT SUITE: the aterm-objc integration files that only read the
//! runtime (class and method tables, encodings, ABI layout) or the source tree,
//! as modules of ONE test binary.
//!
//! Each file stays at its own path (every `tests/<file>.rs:<line>` citation
//! still resolves) and keeps its own `#[test]`s. What stays OUT, each as its own
//! `[[test]]` in `Cargo.toml` with the reason on the line above it, is every
//! file that owns process-global state a sibling would see: `swizzle` (method
//! implementation swaps on shared classes), `winit_seam` (a process panic hook),
//! `weak` (a `#[global_allocator]`), `protocol_registration` (the runtime's one
//! protocol table), and `exception_containment`, `pools` and
//! `adversary_w2`/`w2b`/`w2c`/`w2d` (self re-exec by `--exact`, panic hooks).
//! `main_thread_bound_drive` has no libtest harness and stays its own target.
//!
//! Sharing one process here is sound because nothing below mutates process
//! state another module reads: the two classes declared (`ATermObjcProbe`,
//! `ATermW3StretProbe`) have unique names, the framework loads are idempotent
//! `OnceLock`s, `conformance_wake_timer` arms timers on its own thread's run
//! loop, and no file re-execs itself, sets a hook, or touches the environment.
//! `encode_offsets` walks every class in the process, so it also sees those
//! two probe classes — whose encodings must satisfy the same "strip deletes
//! offsets only" law as AppKit's.
//!
//! The census support files under `tests/support/` are mounted ONCE here and
//! reached as `crate::…`, so one binary compiles each once and
//! `sdk_consts`'s own parser test runs once. `winit_seam`, its own target,
//! keeps its own mounts.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-objc --test objc_static_suite -- <file>::`.

#[cfg(target_os = "macos")]
#[path = "support/appkit.rs"]
mod appkit;
#[cfg(target_os = "macos")]
#[path = "support/sdk_consts.rs"]
mod sdk_consts;
#[cfg(target_os = "macos")]
#[path = "support/winit_fork.rs"]
mod winit_fork;

mod abi;
mod blocks;
mod conformance_wake_timer;
mod declared_class;
mod encode_offsets;
mod gui_appkit_constants;
mod gui_sent_prototypes;
mod main_thread_bound;
mod objc2_exit_condition;
mod runtime;
mod send_prototype_census;
mod stret_declared;
mod winit_seam_constants;
mod winit_sent_prototypes;
