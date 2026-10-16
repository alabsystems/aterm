// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The atpkg-keys integration files as modules of ONE test binary: each file
//! keeps its path (so every citation of `crates/atpkg-keys/tests/<file>.rs`
//! still resolves) and its own `#[test]`s.
//!
//! `provision_cli` drives the binary on a real terminal, from `pty`, the one pty
//! fixture. It hands the child its slave by descriptor, so no test calls
//! `ptsname(3)` and no lock guards its static buffer. Nothing below sets the environment, installs a
//! hook or an allocator, or keeps a process global; every child the tests
//! start gets its environment on its own command.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p atpkg-keys --test keys_suite -- <file>::`.

mod owner_to_client;
mod paper_master_to_client;
mod provision_cli;
#[cfg(unix)]
mod pty;
mod roster_redo_model;
