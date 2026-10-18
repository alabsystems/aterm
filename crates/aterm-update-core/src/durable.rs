// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's flushes, in one place: the boot sentinel's record before its rename, and
//! its directory after it ([`crate::sentinel`]). Each finishes through aterm-durable:
//! [`sync_all`] is its DURABLE arm, kept here for this crate's unit-test arm below, and
//! [`finish`] picks between it and [`aterm_durable::ordered`].

use aterm_durable::Flush;
use std::fs::File;
use std::path::Path;

/// Make one open file or directory durable: [`aterm_durable::durable`], which is
/// `fcntl(F_FULLFSYNC)` (a drive-cache flush) on Apple targets and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// [`sync_all`] under this crate's unit tests on Apple targets: a plain `fsync(2)`
/// ([`aterm_durable::seam::durable_fsync`], which the counting seam records as durable). It
/// flushes at the same point and in the same order and drops only the drive-cache barrier,
/// which no test can observe and which stalls the whole volume for every test binary running
/// beside this one (beside three other unit-test runs: 0.60 s instead of ~0.9 s). Integration
/// tests, shipped builds and dependent crates keep `F_FULLFSYNC`.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::seam::durable_fsync(file)
}

/// Finish a write to `file` with `flush`: [`Flush::Durable`] through [`sync_all`] (and its
/// test arm), [`Flush::Ordered`] through [`aterm_durable::ordered`].
pub(crate) fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}

/// [`finish`] on the directory `dir`: what makes a rename in it durable, or ordered.
pub(crate) fn sync_dir(dir: &Path, flush: Flush) -> std::io::Result<()> {
    finish(&File::open(dir)?, flush)
}
