// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish`: packs, signs, and publishes atpkg builds and channel indexes.

pub mod adopt;
pub mod api;
pub mod artifact;
pub mod canary;
pub mod channel;
pub mod cli;
mod compiler_context;
pub mod ctx;
pub mod error;
pub mod git;
#[cfg(unix)]
pub mod harness_local;
pub mod host;
pub mod hosts_check;
pub mod identity;
pub mod lane;
pub mod pins;
pub mod programs;
pub mod project_toolchain;
pub mod quad;
/// Test fixtures: never in a release build, which the lane's snapshot is.
#[cfg(any(test, debug_assertions))]
pub mod testkit;
pub mod triple;
