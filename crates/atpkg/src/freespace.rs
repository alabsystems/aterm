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
pub(crate) mod pinned {
    use std::path::{Path, PathBuf};

    static PINS: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

    /// [`super::available_bytes`] answers its free bytes for every path under its root until it
    /// drops.
    pub(crate) struct Pin(PathBuf);

    /// Pin `free` bytes free for every path under `root`.
    pub(crate) fn pin(root: &Path, free: u64) -> Pin {
        pins().push((root.to_path_buf(), free));
        Pin(root.to_path_buf())
    }

    /// The bytes the latest pin over `start` names.
    pub(super) fn under(start: &Path) -> Option<u64> {
        pins()
            .iter()
            .rev()
            .find(|(root, _)| start.starts_with(root))
            .map(|&(_, free)| free)
    }

    fn pins() -> std::sync::MutexGuard<'static, Vec<(PathBuf, u64)>> {
        PINS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    impl Drop for Pin {
        fn drop(&mut self) {
            let mut pins = pins();
            if let Some(at) = pins.iter().rposition(|(root, _)| *root == self.0) {
                pins.remove(at);
            }
        }
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
