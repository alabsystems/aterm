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

/// Create `dir` (and parents) if absent, force its mode to `0700`, and VERIFY
/// it is owned by us and not group/other-writable before returning success —
/// the control socket's private-dir rule (`ensure_private_dir` in aterm-gui's
/// `control_auth_unix`), shared here so the session-identity tree
/// (`<state>/identities/<name>/`) is provisioned under the same fail-closed
/// gate: forcing the mode is not enough on its own, because a directory
/// another user pre-created keeps its owner through `set_permissions`, and
/// that user could still swap files in. Idempotent: an existing 0700 dir of
/// ours is simply verified. Non-Unix: create only (no mode bits to verify).
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    create_dir_restricted(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let meta = std::fs::metadata(dir)?;
        let uid = current_uid();
        if !dir_safe_for_private_write(uid, meta.uid(), meta.mode()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: must be owned by uid {uid} and not group/other-writable",
                    dir.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid() is always safe — no failure mode, no args.
    unsafe { libc_getuid() }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
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

    /// `ensure_private_dir` creates 0700 (parents included), tightens a looser
    /// existing dir of ours to 0700, and is idempotent — the provisioning every
    /// identity dir and agent subdir goes through.
    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_creates_0700_tightens_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "aterm-fs-restricted-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dir = root.join("identities").join("worker");
        ensure_private_dir(&dir).expect("created");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(
            mode(&root.join("identities")),
            0o700,
            "parents are 0700 too"
        );
        ensure_private_dir(&dir).expect("idempotent");
        assert_eq!(mode(&dir), 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).expect("an existing dir of ours is tightened, not refused");
        assert_eq!(mode(&dir), 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn private_write_rejects_group_or_other_writable() {
        assert!(!dir_safe_for_private_write(501, 501, 0o040775));
        assert!(!dir_safe_for_private_write(501, 501, 0o040757));
        assert!(!dir_safe_for_private_write(501, 501, 0o040722));
    }
}
