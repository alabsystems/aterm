// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stand-in for `crc32fast-1.5.0/src/specialized/pclmulqdq.rs`, so the verbatim
//! `mod pclmulqdq; pub use self::pclmulqdq::State;` arm has a real file.

/// The type the arm re-exports; its name is what a leaked second item would
/// collide with.
pub struct State;

impl State {
    /// Which arm of the chain produced this `State`.
    pub fn origin() -> &'static str {
        "pclmulqdq"
    }
}
