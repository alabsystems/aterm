// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! macOS atomic app placement, shared by the updater and bootstrap installer helper.

use std::ffi::CString;
use std::io;
use std::path::Path;

/// Atomically exchange the directory entries `a` and `b` via `renamex_np` with `RENAME_SWAP`
/// (APFS, same volume): neither path is ever missing. The caller checks `same_volume` first.
///
/// # Errors
/// Either path contains a NUL, or the kernel refuses the exchange (including missing paths,
/// permissions, different volumes or missing filesystem support).
pub fn rename_swap(a: &Path, b: &Path) -> io::Result<()> {
    let ca = cpath(a)?;
    let cb = cpath(b)?;
    // SAFETY: `ca` and `cb` are NUL-terminated C strings that outlive the call; RENAME_SWAP is
    // a documented flag; failure is -1/errno.
    let rc = unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Promote a directory only into an absent name; never replace a raced destination.
///
/// # Errors
/// Either path contains a NUL, or the kernel refuses the exclusive rename, including an
/// occupied destination, missing source, permissions, volumes or filesystem support.
pub fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (cpath(from)?, cpath(to)?);
    // SAFETY: both paths are live NUL-terminated strings; RENAME_EXCL refuses an occupied name.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Build a NUL-terminated C string from a path, rejecting embedded NULs.
fn cpath(p: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}
