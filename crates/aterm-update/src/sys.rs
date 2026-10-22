// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The APFS atomic directory exchange (`renamex_np` with `RENAME_SWAP`) behind the `.app`
//! swap, which has no safe std wrapper. The portable primitives (`FileLock`, `same_volume`)
//! live in `aterm-update-core`.

use std::ffi::CString;
use std::io;
use std::path::Path;

/// Atomically exchange the directory entries `a` and `b` via `renamex_np` with `RENAME_SWAP`
/// (APFS, same volume): neither path is ever missing. The caller checks `same_volume` first.
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

/// Build a NUL-terminated C string from a path, rejecting embedded NULs.
fn cpath(p: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}
