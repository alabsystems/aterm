// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stand-in for `crc32fast-1.5.0/src/specialized/aarch64.rs`.

/// The type the arm re-exports.
pub struct State;

impl State {
    /// Which arm of the chain produced this `State`.
    pub fn origin() -> &'static str {
        "aarch64"
    }
}
