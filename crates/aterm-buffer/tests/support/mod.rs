// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Capability witnesses for the conformance tests. A buffer cap is granted by an
//! `aterm_cap::Authority`, and the mint is sealed behind `launcher-mint`, which
//! this crate enables for its dev-dependency edge only.

use aterm_buffer::{ReadCap, WriteCap};
use aterm_cap::{Authority, Tier};

fn authority() -> Authority {
    // SAFETY: a test process that processes no untrusted input is its own
    // trusted launcher, which is the whole of `root_authority`'s contract.
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
