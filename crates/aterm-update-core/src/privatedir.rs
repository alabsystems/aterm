// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Creating and hardening a per-user private directory (`0700`, owned by us, never
//! group/other-writable) using [`aterm_types::fs_restricted::dir_safe_for_private_write`].

use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(all(test, unix))]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[cfg(all(test, unix))]
thread_local! {
    static CHMOD_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Our effective uid.
#[cfg(unix)]
#[inline]
fn our_uid() -> u32 {
    // SAFETY: getuid() is always-safe (no args, cannot fail).
    unsafe { libc::getuid() }
}

/// Create `dir` (and parents), force mode `0700`, then verify it is owned by us and not
/// group/other-writable, refusing a foreign-owned or shared directory.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    // Refuse a pre-existing symlink before create_dir_all or set_permissions can follow it.
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: update directory is a symlink; refusing", dir.display()),
        ));
    }
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700)?;
    let mut meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update path is not a real directory; refusing",
                dir.display()
            ),
        ));
    }
    let uid = our_uid();
    if meta.uid() == uid && meta.mode() & 0o7777 != 0o700 {
        #[cfg(all(test, unix))]
        CHMOD_CALLS.with(|c| c.set(c.get() + 1));
        set_dir_mode_nofollow(dir, 0o700)?;
        meta = std::fs::symlink_metadata(dir)?;
        if !meta.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: update path is not a real directory; refusing",
                    dir.display()
                ),
            ));
        }
    }
    if aterm_types::fs_restricted::dir_safe_for_private_write(uid, meta.uid(), meta.mode()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: update directory must be owned by uid {uid} and not group/other-writable",
                dir.display()
            ),
        ))
    }
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn set_dir_mode_nofollow(dir: &Path, mode: u32) -> io::Result<()> {
    aterm_uds::private_file::set_dir_mode_nofollow(dir, mode)
}

/// Windows variant: create `dir` and refuse a final component that is a symlink or junction.
#[cfg(windows)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
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

/// Whether `md` is a symlink or directory junction (`FILE_ATTRIBUTE_REPARSE_POINT`).
#[cfg(windows)]
fn is_reparse_point(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_type().is_symlink() || (md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(all(test, unix))]
#[path = "privatedir_tests.rs"]
mod tests;
