// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-folder steps for a root-owned copy of an app bundle ([`super::System::clone_tree`],
//! [`super::System::root_own`], [`super::System::exchange`]): APFS `clonefile(2)`, an owner and
//! mode walk that never follows a link, and `renamex_np`'s atomic swap.

use std::io;
use std::path::Path;

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL"))
}

/// Clone the real directory held open at `from` to the new name `to`, owned by the caller.
/// Source links are refused and a replacement pathname cannot redirect the clone.
///
/// # Errors
/// Unsafe source shape, or the original filesystem error (`EXDEV` remains distinguishable).
pub(super) fn clone(from: &Path, to: &Path) -> Result<(), aterm_uds::app_image::Error> {
    aterm_uds::app_image::clone(from, to)
}

/// Exchange the names `a` and `b` atomically (`renamex_np` with `RENAME_SWAP`): each then names
/// what the other did. Off macOS it refuses (`Unsupported`).
///
/// # Errors
/// The call's error, `ENOENT` when either is missing.
pub(super) fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let (a, b) = (c_path(a)?, c_path(b)?);
        // SAFETY: both pointers are NUL-terminated strings that outlive the call; RENAME_SWAP
        // makes it an exchange of the two existing names.
        if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (a, b);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Make `path` (and, with `tree`, everything below it) `uid:gid` with mode bits no wider than
/// `0755`, never following a link: the group and other write bits, setuid, setgid and sticky are
/// cleared, and any extended ACL is removed, so a bundle the person's account
/// made can carry no grant of its own into a root-owned clone (`ditto`'s copy keeps ACLs and set-id
/// bits). The outer image must be a real directory. Internal links must stay within it;
/// they are re-owned and stripped of ACLs without following them.
///
/// # Errors
/// Unsafe shape or escaping link, or the first failing filesystem/ACL operation.
pub(super) fn own(
    path: &Path,
    uid: u32,
    gid: u32,
    tree: bool,
) -> Result<(), aterm_uds::app_image::Error> {
    aterm_uds::app_image::seal(path, uid, gid, tree)
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
