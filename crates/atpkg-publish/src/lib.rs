// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish`: packs, signs, and publishes atpkg builds and channel indexes.

pub mod api;
pub mod artifact;
pub mod channel;
pub mod cli;
pub mod ctx;
pub mod error;
pub mod git;
pub mod host;
pub mod identity;
pub mod lane;
pub mod pins;
pub mod programs;
pub mod quad;
/// Test fixtures: never in a release build, which the lane's snapshot is.
#[cfg(any(test, debug_assertions))]
pub mod testkit;
pub mod triple;
