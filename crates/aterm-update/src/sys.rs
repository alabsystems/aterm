// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The macOS-specific `libc` call the updater needs that has no safe std wrapper:
//! the APFS atomic directory exchange (`renamex_np` with `RENAME_SWAP`) used for the
//! `.app` swap. A single, documented `unsafe` call — the rest of the crate is safe
//! Rust. The portable primitives (advisory `FileLock`, `same_volume`) live in
//! `aterm-update-core`.

use std::ffi::CString;
use std::io;
use std::path::Path;

/// Atomically exchange the directory entries `a` and `b` via `renamex_np` with
/// `RENAME_SWAP` (APFS, same volume). After success `a` names what was at `b` and
/// vice-versa, with no intermediate window where either path is missing — the
/// swap a self-update needs. Caller must have checked `same_volume` first.
pub fn rename_swap(a: &Path, b: &Path) -> io::Result<()> {
    let ca = cpath(a)?;
    let cb = cpath(b)?;
    // SAFETY: both pointers are valid NUL-terminated C strings (`ca`/`cb` live
    // across the call, so the pointers stay valid); RENAME_SWAP is the documented
    // flag; -1/errno on failure.
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
