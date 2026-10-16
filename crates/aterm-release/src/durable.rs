// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cutter's durability flushes: the barrier its journals, records and Linux-lane
//! artifacts take when they are saved atomically.

use std::fs::File;
use std::path::Path;

/// Make one open file or directory durable: the barrier an atomic publish takes on its
/// temporary before the rename, and on the directory after it. [`File::sync_all`] is
/// `fcntl(F_FULLFSYNC)`, a drive-cache flush, on Apple targets and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// [`sync_all`] under this crate's own tests on Apple targets (`tests/it` mounts this
/// file): a plain `fsync(2)` ([`atpkg::platform::sync_file_contents`]) at the same points.
/// It drops only the drive-cache barrier, which no test observes and which stalls the
/// volume for every other test binary (206 of a run's 284 `F_FULLFSYNC`s; ~20 s to ~14 s).
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    atpkg::platform::sync_file_contents(file)
}

/// [`sync_all`] on the directory `dir`: makes a rename or a new entry in it durable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    sync_all(&File::open(dir)?)
}
