// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Public product-version API over the policy shared with build stamps.

pub use aterm_stamp::release_version::{MAX_CALENDAR_SEQUENCE, NextVersionError, ReleaseVersion};

#[cfg(test)]
#[path = "release_version_tests.rs"]
mod tests;
