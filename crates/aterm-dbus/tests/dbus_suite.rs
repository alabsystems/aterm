// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-dbus integration files as modules of one test binary, so the crate
//! links one test executable instead of one per file. Each file stays at its own
//! path (every `tests/<file>.rs` citation still resolves) and keeps its own
//! `#[test]`s; the recorded corpora's readers (`tests/support/`) are mounted once
//! here and reached as `crate::support`.
//!
//! Sharing one process is sound: every bus here is a private socketpair, a
//! scripted replay or a private `dbus-daemon` on a scratch socket, and nothing
//! sets the environment. `connection_identity` re-runs this binary on one of its
//! own tests (`--exact connection_identity::<test>`) with an environment of its
//! own, so the child it judges is a process of its own.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own never compiles.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-dbus --test dbus_suite -- <file>::`.

mod support;

mod conn_faults;
mod connection_identity;
mod live_bus;
mod mutations;
mod zbus_messages;
mod zbus_session;
mod zvariant_values;
