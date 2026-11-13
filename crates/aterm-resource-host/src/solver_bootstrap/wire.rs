// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared strict records describe this existing attempt, never new authority.
pub(super) use trust_resource_controller::solver_bootstrap::{FRAME_LEN as LEN, Frame};

pub(super) fn invalid() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "private solver bootstrap refused",
    )
}
