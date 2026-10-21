// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-wayland integration files as modules of one test binary, so the
//! crate links one test executable instead of one per file. Each file stays at
//! its own path and keeps its own `#[test]`s and its own `#![cfg]`: `generated`
//! and `tables` run on any host, `cursor_layout`, `full_socket` and
//! `live_wayland` on Linux only. The generator's own code (`scan/src`, built
//! outside the workspace) is compiled here, at the root its `crate::xml`
//! paths name, and `generated` reaches it as `crate::generate`.
//!
//! Sharing one process is sound: `generated` and `tables` are pure,
//! `cursor_layout` compiles its C probe in a scratch directory, `full_socket`
//! speaks over a socket pair of its own, and `live_wayland` opens its own
//! connections; nothing sets the environment.
//! `tests/live_seat.rs` stays a binary of its own: it holds Mutter's one seat,
//! and a toplevel `live_wayland` maps there at the same time could take the
//! keyboard focus its keys are aimed at.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own never compiles.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-wayland --test wayland_suite -- <file>::`.

#[allow(dead_code, reason = "the generator's binary entry is not used here")]
#[path = "../scan/src/generate.rs"]
mod generate;
#[allow(dead_code, reason = "as above")]
#[path = "../scan/src/xml.rs"]
mod xml;

mod cursor_layout;
mod full_socket;
mod generated;
mod live_wayland;
mod tables;
