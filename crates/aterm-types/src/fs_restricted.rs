// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Filesystem helpers with restricted permissions (mode 0o700; CWE-276).

use std::io;
use std::path::Path;

/// Create a directory (and parents) with mode 0o700 on Unix; elsewhere plain `create_dir_all`.
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

/// The mode a missing ancestor of one of aterm's own directories is born with
/// ([`create_dir_all_unshared`]): what a `022` umask gives it, and never the group or
/// other write bit a plain `create_dir_all` leaves under `002` (a user-private-group
/// login's default) or `000`.
pub const ANCESTOR_DIR_MODE: u32 = 0o755;

/// Create `dir` and its missing ancestors so that no directory this call makes is group-
/// or other-writable, whatever the umask: each missing ancestor is born
/// [`ANCESTOR_DIR_MODE`] and `dir` itself `leaf_mode`, both narrowed further by the umask
/// (a tighter umask stays tighter). A directory that already exists is left exactly as it
/// is, mode and `st_ctime` included; an existing `0775` ancestor stays `0775`. Follows a
/// link as `create_dir_all` does, so a caller that refuses one checks before and after.
///
/// `create_dir_all` makes every missing component `0777 & ~umask`: under umask `002` the
/// first atpkg pass made `~/.local/share/aterm` `0775` on the way to its `0700` prefix,
/// and `targo trust` refuses every toolchain installed below a group-writable directory.
/// Non-Unix: `create_dir_all` (no mode to ask for).
pub fn create_dir_all_unshared(dir: &Path, leaf_mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;
        if dir.as_os_str().is_empty() {
            return Ok(());
        }
        // `create_dir_all`'s own order: the leaf first, so an existing directory costs one
        // `mkdir`; its ancestors only when the leaf's parent is missing.
        match DirBuilder::new().mode(leaf_mode).create(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) if dir.is_dir() => return Ok(()),
            Err(e) => return Err(e),
        }
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            DirBuilder::new()
                .recursive(true)
                .mode(ANCESTOR_DIR_MODE)
                .create(parent)?;
        }
        match DirBuilder::new().mode(leaf_mode).create(dir) {
            Err(_) if dir.is_dir() => Ok(()),
            made => made,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = leaf_mode;
        std::fs::create_dir_all(dir)
    }
}

/// Create `dir` (and parents) if absent, force mode `0700`, and verify it is ours and not
/// group/other-writable: the control socket's private-dir rule, shared by the session-identity
/// tree. Forcing the mode alone is not enough, because a directory another user pre-created
/// keeps its owner. Idempotent. Non-Unix: create only.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    create_dir_restricted(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // Write the mode only when it differs: a same-mode `chmod(2)` still moves `st_ctime`
        // on APFS, and tippy fails when an ancestor of its executable changes ctime mid-run
        // (`~/Library/Application Support/aterm` holds the atpkg store). aterm-update-core's
        // privatedir.rs keeps the same rule.
        if !std::fs::metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o700) {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
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

/// Whether the caller (`uid`) may write private files into a directory owned by `owner_uid`
/// with `st_mode` bits `mode`: owned by the caller and not group/other-writable. A directory
/// another user can write into lets them swap in a symlink or pre-create the file (CWE-379);
/// read bits are the caller's business (a 0755 home subdir passes, /tmp does not).
#[must_use]
pub const fn dir_safe_for_private_write(uid: u32, owner_uid: u32, mode: u32) -> bool {
    owner_uid == uid && mode & 0o022 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a directory we own that nobody else can write is safe; group or
    /// other READ is the caller's choice.
    #[test]
    fn private_write_needs_our_ownership_and_no_foreign_write_bit() {
        for (uid, owner, mode, safe) in [
            (501, 501, 0o040700, true),
            (501, 501, 0o040755, true),
            (0, 0, 0o040700, true),
            // /tmp: root-owned, sticky, world-writable — refused on both counts.
            (501, 0, 0o041777, false),
            (501, 502, 0o040700, false),
            (501, 501, 0o040775, false),
            (501, 501, 0o040757, false),
            (501, 501, 0o040722, false),
        ] {
            assert_eq!(
                dir_safe_for_private_write(uid, owner, mode),
                safe,
                "uid {uid} owner {owner} mode {mode:o}"
            );
        }
    }

    /// `ensure_private_dir` creates 0700 (parents included), tightens a looser dir of ours, and
    /// is idempotent.
    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_creates_0700_tightens_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "aterm-fs-restricted-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
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

    /// An already-`0700` directory of ours gets no `chmod(2)`, so its `st_ctime` holds; a
    /// drifted mode is still forced back to `0700`.
    #[cfg(unix)]
    #[test]
    fn an_already_private_dir_keeps_its_ctime_and_a_drifted_one_is_hardened() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("atypes-privdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let stamp = || {
            let m = std::fs::metadata(&dir).unwrap();
            (m.ctime(), m.ctime_nsec(), m.mode() & 0o7777)
        };
        let before = stamp();
        assert_eq!(before.2, 0o700);
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp(), before, "an unchanged directory was written");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp().2, 0o700, "a drifted mode is hardened");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
