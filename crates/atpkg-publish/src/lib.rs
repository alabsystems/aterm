// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish`: packs, signs, and publishes atpkg builds and channel indexes.
//!
//! It is Unix's, as the owner-side signer it signs with (`atpkg-keys`) is: on Windows the
//! library is empty and the bin says so.
#![cfg(unix)]

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
#[cfg(unix)]
pub mod package_campaign;
pub mod pins;
pub mod programs;
pub mod project_toolchain;
pub mod quad;
/// Test fixtures: never in a release build, which the lane's snapshot is.
#[cfg(any(test, debug_assertions))]
pub mod testkit;
pub mod triple;
