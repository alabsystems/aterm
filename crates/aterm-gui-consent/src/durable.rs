// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's flushes, in one place, for it and for aterm-gui.

pub use aterm_durable::Flush;
use std::fs::File;

/// Make one open file or directory DURABLE.
#[inline]
pub fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Finish a write to `file` with `flush`.
/// # Errors
/// The flush's own error.
#[inline]
pub fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}

/// A plain `fsync(2)`, not `F_FULLFSYNC`.
#[cfg(unix)]
#[inline]
pub fn fsync(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `fsync` on a descriptor this `File` owns for the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
