// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Creating and hardening a per-user private directory under Application Support.
//!
//! Mirrors `aterm-gui`'s `control_auth::ensure_private_dir`, reusing the *same*
//! ownership predicate ([`aterm_types::fs_restricted::dir_safe_for_private_write`])
//! so the two cannot drift on what "private" means: owned by us, mode `0700`, never
//! group/other-writable. Fail closed — a foreign-owned or shared directory is
//! refused rather than written into.

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

/// Create `dir` (and parents), force mode `0700`, then verify it is owned by us
/// and not group/other-writable — refusing a foreign-owned or shared directory
/// (fail closed) exactly as `control_auth::ensure_private_dir` does.
#[cfg(unix)]
// Skip: set_permissions/format on the 0700 support dir — hardened
// permission_change class; creation-time mode + owner check are exactly
// what this fn enforces (the update-atpkg private-dir hardening). The
// capability-contract lane will replace this with dirfd-relative proofs.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    // Refuse a symlink at the target BEFORE touching it: create_dir_all/set_permissions
    // follow a link, so a same-user process that pre-created `Updates` as a symlink to
    // (say) ~/Documents could otherwise capture our chmod, our writes, and — worse —
    // `Staging::clear()`'s recursive delete. lstat and fail closed (F16).
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: update directory is a symlink; refusing", dir.display()),
        ));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // Re-check with lstat (NOT metadata, which would follow a link swapped in after
    // creation): the final component must be a real, we-owned, non-shared directory.
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

/// Windows variant: create the directory and verify the final component is a
/// real directory (the symlink refusal is kept — `symlink_metadata` works here
/// too). Per-user `%LOCALAPPDATA%` ACLs are the confidentiality boundary; POSIX
/// mode/owner semantics are not applicable, so the uid/`0700` hardening above
/// has no analogue. (The updater is inert on Windows; this is compile-only
/// honesty, not a claim of parity.)
#[cfg(windows)]
// Skip: set_permissions/format on the 0700 support dir — hardened
// permission_change class; creation-time mode + owner check are exactly
// what this fn enforces (the update-atpkg private-dir hardening). The
// capability-contract lane will replace this with dirfd-relative proofs.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    // Refuse a reparse point at the target BEFORE touching it, mirroring the unix
    // fail-closed shape (F16): a pre-created link must not capture our writes. This must
    // reject a directory JUNCTION as well as a symlink — a junction needs no admin and
    // reports `is_symlink() == false` (it carries IO_REPARSE_TAG_MOUNT_POINT), so a
    // symlink-only check lets an attacker-pre-created junction redirect our shim/link writes.
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
    // Re-check after create_dir_all closes a TOCTOU where a junction is swapped in between
    // the pre-check and here: the final component must be a real directory, not a reparse point.
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

/// Whether `md` (from `symlink_metadata`) is any reparse point — a symlink OR a directory
/// junction. `FILE_ATTRIBUTE_REPARSE_POINT` (0x400) catches both, unlike `is_symlink()`.
#[cfg(windows)]
fn is_reparse_point(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_type().is_symlink() || (md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}
