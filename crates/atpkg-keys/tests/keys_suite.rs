// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The atpkg-keys integration files as modules of one test binary; each keeps its path and
//! its own `#[test]`s. `autotests = false` in `Cargo.toml`, so a `tests/*.rs` file listed
//! neither here nor as its own `[[test]]` never compiles.
//!
//! Nothing here sets the environment, installs a hook or an allocator, or keeps a process
//! global: every child the tests start gets its environment on its own command.
//!
//! Run one file's tests with
//! `targo --unverified test -p atpkg-keys --test keys_suite -- <file>::`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod owner_to_client;
mod paper_master_to_client;
mod provision_cli;
#[cfg(unix)]
mod pty;
mod roster_redo_model;
