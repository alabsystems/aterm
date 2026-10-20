// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-x11 integration files as modules of one test binary, so the crate
//! links one test executable instead of one per file. Each file stays at its own
//! path and keeps its own `#[test]`s and its own `#![cfg]`: the codec files run
//! on any little-endian host, `live_x11` and `xlib_layout` on Linux only. The
//! recordings' readers (`tests/support/`) are mounted once here and reached as
//! `crate::support`.
//!
//! Sharing one process is sound: the codec files are pure, `xlib_layout`
//! compiles its C probe in a scratch directory, and `live_x11` opens its own
//! connections to `$DISPLAY`; nothing sets the environment.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own never compiles.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-x11 --test x11_suite -- <file>::`.

#[cfg(target_endian = "little")]
mod support;

mod icccm_xrm;
mod live_x11;
mod replies;
mod requests;
mod xlib_layout;
