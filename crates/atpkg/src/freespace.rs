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
    let mut p = start;
    loop {
        if p.exists() {
            return crate::platform::volume_free_bytes(p);
        }
        p = p.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_to_existing_ancestor() {
        let d = std::env::temp_dir();
        let ghost = d.join("atpkg-freespace-does/not/exist");
        // A raw statvfs of a missing path fails; the walk-up resolves it to the existing
        // ancestor. Byte equality is not asserted: free space moves as other tests write.
        assert!(
            available_bytes(&d).is_some(),
            "the existing ancestor queries fine"
        );
        assert!(
            available_bytes(&ghost).is_some(),
            "a non-existent path resolves to its existing ancestor's volume"
        );
    }
}
