// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Filesystem helpers with restricted permissions (mode 0o700).
//!
//! Canonical implementation shared across all crates (CWE-276, #5815).

use std::io;
use std::path::Path;

/// Create a directory (and parents) with mode 0o700 on Unix.
///
/// On non-Unix platforms, falls back to `create_dir_all` (no mode bits).
pub fn create_dir_restricted(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;
        DirBuilder::new().recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Pure predicate: may the caller (`uid`) write private files into a
/// directory owned by `owner_uid` with permission bits `mode` (`st_mode`)?
///
/// Safe means owned by the caller AND not writable by group or other: a
/// directory another local user can write into lets them swap the target for
/// a symlink between our check and our write, or pre-create the file
/// (CWE-379). Group/other READ bits are the caller's own business — an
/// explicit 0755 home subdir is honoured; /tmp (root-owned, world-writable,
/// sticky) is not. Hosts stat the directory and pass the bits in; the
/// decision itself stays platform-free and testable.
#[must_use]
pub fn dir_safe_for_private_write(uid: u32, owner_uid: u32, mode: u32) -> bool {
    owner_uid == uid && mode & 0o022 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_write_accepts_owned_unwritable_by_others() {
        assert!(dir_safe_for_private_write(501, 501, 0o040700));
        // Group/other read is the caller's choice; only WRITE is fatal.
        assert!(dir_safe_for_private_write(501, 501, 0o040755));
        assert!(dir_safe_for_private_write(0, 0, 0o040700));
    }

    #[test]
    fn private_write_rejects_foreign_owner() {
        // /tmp: root-owned, sticky, world-writable — refused on both counts.
        assert!(!dir_safe_for_private_write(501, 0, 0o041777));
        assert!(!dir_safe_for_private_write(501, 502, 0o040700));
    }

    #[test]
    fn private_write_rejects_group_or_other_writable() {
        assert!(!dir_safe_for_private_write(501, 501, 0o040775));
        assert!(!dir_safe_for_private_write(501, 501, 0o040757));
        assert!(!dir_safe_for_private_write(501, 501, 0o040722));
    }
}
