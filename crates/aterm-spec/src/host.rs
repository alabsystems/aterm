// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test the HOST cannot run ends typed, never as a pass: a GPU, a platform
//! font or a privilege the machine lacks decides nothing about the code.

/// The words that open a refusal by the machine. A test log whose every
/// failure carries them is recorded as COULD NOT RUN, not FAIL (aterm-verify's
/// `libtest::environment_refusals`, which keeps its own copy: that crate
/// depends on nothing).
pub const COULD_NOT_RUN: &str = "aterm-gate: COULD NOT RUN";

/// End the calling test as COULD NOT RUN: `why` names what this host lacks
/// and, where there is one, the fix.
///
/// # Panics
///
/// Always: a panic is the one outcome libtest records beside pass and fail.
#[track_caller]
pub fn could_not_run(why: impl std::fmt::Display) -> ! {
    panic!("{COULD_NOT_RUN} — {why}")
}
