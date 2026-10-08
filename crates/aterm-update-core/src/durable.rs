// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's durability flushes, in one place: the boot sentinel's record before its
//! rename, and its directory after it ([`crate::sentinel`]).

use std::fs::File;
use std::path::Path;

/// Make one open file or directory DURABLE. It is [`File::sync_all`], which on Apple
/// targets is `fcntl(F_FULLFSYNC)`, a drive-cache flush, and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// [`sync_all`] under this crate's own unit tests on Apple targets: a plain `fsync(2)`,
/// retried on `EINTR` as std retries its `F_FULLFSYNC`. It is still a real flush, made at
/// the same point and in the same order, so a test sees the same files, renames and
/// errors. What it drops is the drive-cache barrier. No test can observe that barrier,
/// because it only orders the disk against a power loss. It also stalls the whole VOLUME
/// while it runs, so every test binary the gate runs beside this one waits behind it for
/// its own temp-dir metadata. Measured 2026-10-06: one `aterm-update-core --lib` run made
/// 36 `F_FULLFSYNC`s, every one of them the sentinel's record or its directory, and with
/// this arm it makes none. Alone the run took 0.55 s either way; beside the aterm-primer,
/// aterm-release and aterm-agent unit-test runs it took 0.60 s instead of 0.89-0.92 s.
/// Off Apple targets `sync_all` already is `fsync(2)`, so the arm above stays.
/// Integration tests, every shipped build and every crate that depends on this one
/// (`aterm-update`'s tests make 811 of these through [`crate::sentinel`]) compile it too.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    loop {
        // SAFETY: `fsync` on a descriptor this `File` owns for the call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// [`sync_all`] on the directory `dir`: what makes a rename in it durable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    sync_all(&File::open(dir)?)
}
