// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GATE'S OWN SUITES, ONE BINARY: each file below is a module — one link,
//! one start, one slot in the test run. Nothing in them is
//! process-global: no `set_var`, no `set_current_dir`, no shared state;
//! every fixture makes its own temp root; no test re-executes this binary by
//! name. (The statics there are caches of work done once per process — the
//! shared stand-in files, `common::run_once`, `common::shared_copy`,
//! `common::git_program` — which change what a test costs, never what it sees.)
//!
//! WHAT IT DOES NOT BUY: wall time. Measured under load, `targo test -p
//! aterm-verify` was unchanged (308-329 s against 309-346 s split), but this
//! binary alone (280-297 s) is a longer pole than the longest split suite
//! (185-188 s) under the gate's two-at-a-time pool. Split it back if it
//! becomes the run's tail (the test stage's slowest-tests table shows it).
//!
//! THE SHARED FIXTURE HALF, `tests/common/mod.rs`, is mounted ONCE, here:
//! the two suites that use it say `use crate::common;`. Their own `mod
//! common;` would resolve to `tests/<suite>/common.rs` under a `mod <suite>;`
//! and not compile (E0583). It is `cfg(unix)` because its only users are.
//!
//! `autotests = false` in Cargo.toml, so a new `tests/*.rs` file (or
//! `tests/<dir>/main.rs`) compiles only once it is a `mod` below.

#[cfg(unix)]
mod common;

mod cited_test_targets;
mod differential_soundness;
mod environment_contract;
mod gate_contract;
mod handback_lane_keys;
mod profile_pin;
mod verify_sh_pin;
