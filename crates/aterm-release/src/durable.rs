// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cutter's durability flushes, in one place: the resume journal's atomic save
//! ([`crate::publish`]), the release target's owner record and the verified binary
//! it publishes ([`crate::buildplan`]), and the Linux lane's artifact copy and
//! receipt.

use std::fs::File;
use std::path::Path;

/// Make one open file or directory DURABLE: the barrier an atomic publish takes on its
/// temporary before the rename, and on the directory after it. It is
/// [`File::sync_all`], which on Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache
/// flush, and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// [`sync_all`] under this crate's own tests on Apple targets (the inline unit tests run
/// in `tests/it`, which mounts this file): a plain `fsync(2)`, retried on `EINTR` as std
/// retries its `F_FULLFSYNC` ([`atpkg::platform::sync_file_contents`]). It is still a
/// real flush, made at the same point and in the same order, so a test sees the same
/// files, renames and errors. What it drops is the drive-cache barrier. No test can
/// observe that barrier, because it only orders the disk against a power loss. It also
/// stalls the whole VOLUME while it runs, so every test binary the gate runs beside this
/// one waits behind it for its own temp-dir metadata. MEASURED: it drops 206 of a
/// `tests/it` run's 284 `F_FULLFSYNC`s, and the run beside other unit-test binaries
/// from ~20 s to ~14 s. Off Apple targets `sync_all` already is `fsync(2)`, so the
/// arm above stays. The cutter's own build compiles it too.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    atpkg::platform::sync_file_contents(file)
}

/// [`sync_all`] on the directory `dir`: what makes a rename or a new entry in it
/// durable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    sync_all(&File::open(dir)?)
}
