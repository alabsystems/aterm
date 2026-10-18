// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROCESS SUITE: the aterm-objc integration files whose subject is the
//! process itself, as modules of ONE test binary that dispatches by test name.
//!
//! Every fatal case re-runs this binary on its one test (`--exact
//! <module>::<test>`, `support/reexec.rs`) with an environment variable that
//! tells the test to run its fatal body, then reads the child's signal and
//! stderr: an `NSException` that must abort rather than be contained, or the
//! default containment line (`exception_containment`), and the misuses that
//! must abort by name, or must not (`fatal_paths`).
//!
//! Apart from `objc_static_suite` because `exception_containment` installs the
//! process-wide containment sink, which every contained raise in the binary
//! then reports through; nothing here raises in the parent but those tests.
//! Each `[[test]]` that owns a process-global the parent would carry (a
//! swizzle, a panic hook, a global allocator, the protocol table) stays its
//! own binary (`Cargo.toml`).
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-objc --test objc_process_suite -- <file>::`.

#[cfg(target_os = "macos")]
#[path = "support/reexec.rs"]
mod reexec;

mod exception_containment;
mod fatal_paths;
