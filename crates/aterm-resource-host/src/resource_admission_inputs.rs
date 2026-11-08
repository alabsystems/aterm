// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local observations of original native inputs have a hang detector, not a latency contract.

use std::{
    io,
    time::{Duration, Instant},
};
use trust_resource_provenance::{process::Identity, program::HeldProgram};

pub(super) fn deadline(started: Instant) -> Instant {
    started + Duration::from_secs(60)
}

pub(super) fn validate(held: &HeldProgram, started: Instant) -> io::Result<()> {
    held.validate(deadline(started))
}

pub(super) fn validate_loaded(
    held: &HeldProgram,
    child: Identity,
    started: Instant,
) -> io::Result<()> {
    held.validate_loaded(child, deadline(started))
}
