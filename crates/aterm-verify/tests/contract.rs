// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CLI's own integration suites, one binary: each file below is a module.
//! Nothing in them is process-global (no `set_var`, no `set_current_dir`);
//! every fixture makes its own temp root.
//!
//! `autotests = false` in Cargo.toml, so a new `tests/*.rs` file compiles only
//! once it is a `mod` below.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the verify CLI spawns its checks"
)]

#[cfg(unix)]
mod common;

mod arena_interrupt;
mod conformance_end_by_proof;
mod gate_contract;
