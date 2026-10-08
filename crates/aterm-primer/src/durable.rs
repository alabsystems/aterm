// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's durability flush, in one place: the barrier every atomic write of a
//! context or settings file takes on its temporary before the rename
//! ([`crate::write_atomically`]), and the one a backup takes before it counts.

use std::fs::File;

/// Make one open file DURABLE. It is [`File::sync_all`], which on Apple targets is
/// `fcntl(F_FULLFSYNC)`, a drive-cache flush, and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// [`sync_all`] under this crate's own unit tests on Apple targets: a plain `fsync(2)`,
/// retried on `EINTR` as std retries its `F_FULLFSYNC`. It is still a real flush, made
/// at the same point and in the same order, so a test sees the same files, renames and
/// errors. What it drops is the drive-cache barrier. No test can observe that barrier,
/// because it only orders the disk against a power loss. It also stalls the whole VOLUME
/// while it runs, so every test binary the gate runs beside this one waits behind it for
/// its own temp-dir metadata. Measured 2026-10-06: one `aterm-primer --lib` run made 212
/// `F_FULLFSYNC`s, the atomic writes' and the backups', and with this arm it makes none.
/// It took 0.07-0.08 s instead of 0.98-1.28 s alone, and 0.28-0.29 s instead of
/// 2.95-3.14 s beside the aterm-update-core, aterm-release and aterm-agent unit-test
/// runs. Off Apple targets `sync_all` already is `fsync(2)`, so the arm above stays.
/// Integration tests, every shipped build and every crate that depends on this one
/// compile it too. The crate is std-only, so the arm declares the one libSystem call it
/// makes rather than depending on `libc`.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `fsync(2)` takes a descriptor by value and dereferences no pointer; for
    // a descriptor that is not open it fails with `EBADF`.
    unsafe extern "C" {
        safe fn fsync(fd: std::ffi::c_int) -> std::ffi::c_int;
    }
    loop {
        if fsync(file.as_raw_fd()) == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
