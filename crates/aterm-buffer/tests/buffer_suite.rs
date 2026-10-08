// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-buffer integration files as modules of ONE test binary. The
//! capability witnesses in `support/` are mounted once, here, and each file
//! reaches them as `crate::support`; each file keeps its path and its own
//! `#[test]`s. None of them sets the environment, installs a hook or an
//! allocator, keeps a process global or touches the filesystem.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-buffer --test buffer_suite -- <file>::`.

mod support;

mod conformance_cursor;
mod conformance_eventlog;
mod conformance_evict_full;
mod conformance_kernel;
mod conformance_snapshot;
mod conformance_subscribe;
mod conformance_temporal;
mod conformance_transact;
mod public_edit_compat;
