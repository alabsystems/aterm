// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The OS-edge helper for disk preflight (§9) — the free-space query [`crate::cost`]
//! deliberately excludes so its decision core stays pure.
//!
//! Preflight is a **safety net, not a security gate**: [`available_bytes`] returns `None`
//! on any query failure and callers then fail **OPEN** (an FFI hiccup must never wedge an
//! install). The only hard refusal is a genuine, measured shortfall. The per-volume query
//! itself ([`crate::platform::volume_free_bytes`]: `statvfs` on Unix,
//! `GetDiskFreeSpaceExW` on Windows) is the one OS-specific edge; the walk-up here is
//! portable.

use std::path::Path;

/// Free bytes on the volume that will hold `start`. If `start` does not exist yet (a
/// not-yet-created staging/build dir), walks up to its nearest existing ancestor and
/// queries that (same volume). `None` on any query failure — callers fail OPEN.
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
    fn available_bytes_of_existing_dir_is_positive() {
        let d = std::env::temp_dir();
        let avail = available_bytes(&d).expect("statvfs of a real dir should succeed");
        assert!(avail > 0, "a mounted volume reports some free space");
    }

    #[test]
    fn walks_up_to_existing_ancestor() {
        let d = std::env::temp_dir();
        let ghost = d.join("atpkg-freespace-does/not/exist");
        // A raw statvfs of a non-existent path fails (None); the walk-up makes the query
        // succeed by resolving to the nearest existing ancestor (same volume). So the ghost
        // path yields Some — that Some IS the walk-up. (Exact byte equality with the ancestor
        // is deliberately NOT asserted: free space fluctuates as other tests touch the temp
        // volume, which would make an equality check flaky.)
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
