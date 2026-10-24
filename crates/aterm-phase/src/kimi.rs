// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kimi Code (`kimi`) CLI harness version and screen metadata.

/// Major component of the Kimi Code (`kimi`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MAJOR: u16 = 0;

/// Minor component of the Kimi Code (`kimi`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MINOR: u16 = 1;

/// Version of the Kimi Code (`kimi`) harness, tracked independently per harness starting at `"0.1"`.
pub const HARNESS_VERSION: &str = "0.1";

/// Structured incremental numeric version of the Kimi Code (`kimi`) harness.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(
    HARNESS_VERSION_MAJOR,
    HARNESS_VERSION_MINOR,
    HARNESS_VERSION,
);
