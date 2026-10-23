// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Capability witnesses for the conformance tests.

use aterm_buffer::{ReadCap, WriteCap};
use aterm_cap::{Authority, Tier};

fn authority() -> Authority {
    // SAFETY: a test process with no untrusted input is its own trusted launcher.
    unsafe { Authority::root_authority() }
}

#[allow(dead_code, reason = "not every conformance file reads")]
pub fn read_cap() -> ReadCap {
    authority().grant(Tier::Trusted)
}

#[allow(dead_code, reason = "not every conformance file writes")]
pub fn write_cap() -> WriteCap {
    authority().grant(Tier::Trusted)
}
