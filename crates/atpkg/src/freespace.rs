// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The free-space query for the disk preflight, the OS edge [`crate::cost`] leaves out.
//!
//! Preflight is a safety net, not a security gate: a failed query is `None` and callers fail
//! open, so only a measured shortfall refuses an install.

use std::path::Path;

/// Free bytes on the volume that will hold `start`, queried at its nearest existing ancestor
/// when `start` does not exist yet. `None` on any query failure.
#[must_use]
pub fn available_bytes(start: &Path) -> Option<u64> {
    #[cfg(test)]
    if let Some(free) = pinned::under(start) {
        return Some(free);
    }
    let mut p = start;
    loop {
        if p.exists() {
            return crate::platform::volume_free_bytes(p);
        }
        p = p.parent()?;
    }
}

/// A test's stand-in for a volume's free bytes, by the tree it covers, so tests side by side
/// each see only their own.
#[cfg(test)]
#[path = "freespace_pinned_tests.rs"]
pub(crate) mod pinned;

#[cfg(test)]
#[path = "freespace_tests.rs"]
mod tests;
