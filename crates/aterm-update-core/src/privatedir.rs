// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Creating and hardening a per-user private directory under Application Support.
//!
//! Shares the ownership predicate ([`aterm_types::fs_restricted::dir_safe_for_private_write`])
//! with `aterm-gui`'s control-socket hardening, so the two cannot drift on what private means:
//! owned by us, mode `0700`, never group/other-writable. A foreign-owned or shared directory
//! is refused rather than written into.

use std::io;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// Our effective uid.
#[cfg(unix)]
fn our_uid() -> u32 {
    // SAFETY: getuid() is always-safe (no args, cannot fail).
    unsafe { libc::getuid() }
}

/// Create `dir` (and parents), force mode `0700`, then verify it is owned by us and not
/// group/other-writable, refusing a foreign-owned or shared directory.
#[cfg(unix)]
// Skip: set_permissions/format on the 0700 dir; the mode and owner checks are what this fn
// enforces.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    // Refuse a symlink before touching the target: create_dir_all and set_permissions follow
    // it, so a link pre-created by a same-user process would capture our chmod, our writes and
    // the updater's recursive deletes.
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: update directory is a symlink; refusing", dir.display()),
        ));
    }
    std::fs::create_dir_all(dir)?;
    // Write the mode only when it differs: a same-mode `chmod(2)` still moves `st_ctime` on
    // APFS, and tippy refuses to run when an ancestor of its executable changed identity, so
    // re-hardening the atpkg prefix on every store-lock acquisition broke tippy from the store.
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o700) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // lstat, not metadata, which would follow a link swapped in after creation.
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update path is not a real directory; refusing",
                dir.display()
            ),
        ));
    }
    if aterm_types::fs_restricted::dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update directory must be owned by uid {} and not group/other-writable",
                dir.display(),
                our_uid()
            ),
        ))
    }
}

/// Windows variant: create the directory and refuse a final component that is not a real
/// directory. Per-user `%LOCALAPPDATA%` ACLs are the confidentiality boundary; there is no
/// uid/`0700` analogue.
#[cfg(windows)]
// Skip: format over the refusal messages; the reparse-point checks are what this fn enforces.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    // Refuse a reparse point before touching the target. A junction needs no admin and
    // reports `is_symlink() == false`, so a symlink-only check would let a pre-created one
    // redirect our writes.
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && is_reparse_point(&md)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update directory is a symlink/junction; refusing",
                dir.display()
            ),
        ));
    }
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    // Re-check after create_dir_all: a junction swapped in since the pre-check is refused.
    if !meta.file_type().is_dir() || is_reparse_point(&meta) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update path is not a real directory; refusing",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// Whether `md` (from `symlink_metadata`) is a symlink or a directory junction:
/// `FILE_ATTRIBUTE_REPARSE_POINT` catches both, unlike `is_symlink()`.
#[cfg(windows)]
fn is_reparse_point(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_type().is_symlink() || (md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A private directory is left alone (its `st_ctime` does not move), a drifted mode is
    /// forced back to `0700`, and a symlink is refused.
    #[test]
    fn an_already_private_dir_is_not_rewritten_and_a_drifted_one_is_hardened() {
        let dir = std::env::temp_dir().join(format!("atuc-privatedir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let stamp = || {
            let m = std::fs::symlink_metadata(&dir).unwrap();
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

        let link = dir.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err(), "a symlink is refused");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
