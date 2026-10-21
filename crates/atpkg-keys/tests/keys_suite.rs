// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The atpkg-keys integration files as modules of one test binary; each keeps its path and
//! its own `#[test]`s. `autotests = false` in `Cargo.toml`, so a `tests/*.rs` file listed
//! neither here nor as its own `[[test]]` never compiles.
//!
//! Nothing here sets the environment or installs a hook or an allocator: every child the
//! tests start gets its environment on its own command. The one process global is
//! `provision_cli`'s `Once` that settles libSystem's libnotify state before the first fork.
//!
//! Run one file's tests with
//! `targo --unverified test -p atpkg-keys --test keys_suite -- <file>::`.

mod owner_to_client;
mod paper_master_to_client;
mod provision_cli;
#[cfg(unix)]
mod pty;
mod roster_redo_model;
