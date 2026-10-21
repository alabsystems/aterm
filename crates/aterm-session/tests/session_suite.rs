// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-session integration files as modules of one test binary; each keeps its
//! own path and `#[test]`s. With `autotests = false` a `tests/*.rs` file listed
//! neither here nor as a `[[test]]` in Cargo.toml never compiles.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-session --test session_suite -- <file>::`.

mod conformance_authorize;
#[cfg(unix)]
mod immediate_egress;
#[cfg(windows)]
mod windows_input_release;
