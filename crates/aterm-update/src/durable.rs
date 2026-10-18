// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's durability flushes, in one place: the barrier every atomic publish
//! takes on its temporary before the rename (`manifest::write_durable`'s ledgers and
//! markers, the Linux lane's records and executables) and on the directory after it
//! (those, and a completed bundle swap's parents).

use aterm_durable::Flush;
use std::fs::File;
use std::path::Path;

/// Make one open file or directory DURABLE. It is [`aterm_durable::durable`], which on
/// Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache flush, and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// [`sync_all`] under this crate's own unit tests on Apple targets: a plain `fsync(2)`
/// ([`aterm_durable::seam::durable_fsync`], which the counting seam records as durable).
/// It is still a real flush, made at the same point and in the same order, so a test
/// sees the same files, renames and errors. What it drops is the drive-cache barrier.
/// No test can observe that barrier, because it only orders the disk against a power
/// loss. It also stalls the whole VOLUME while it runs, so every test binary running
/// beside this one waits behind it for its own temp-dir metadata. Measured: one
/// `aterm-update --lib` run made 4,492 `F_FULLFSYNC`s, 3,681 of them through this
/// crate (the Linux lane's records and directories, `write_durable`'s ledgers, the
/// swap's parents) and
/// the other 811 the boot sentinel's, which `aterm-update-core` writes.
/// `github::tests::roster_freshness_conformance` made 454 of them, 2.06 s of its 2.64 s
/// alone, and took 0.24 s with this arm. The whole run took 7.0-7.3 s instead of
/// 18.0-20.0 s alone, and 16.3-16.9 s instead of 20.6-27.2 s beside ten spinners (load
/// 30). Off Apple targets `sync_all` already is `fsync(2)`, so the arm above stays.
/// Integration tests and every shipped build compile it too.
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

/// [`finish`] on the directory `dir`: a rename or an exchange in it made durable, or
/// ordered ahead of what follows.
pub(crate) fn sync_dir(dir: &Path, flush: Flush) -> std::io::Result<()> {
    finish(&File::open(dir)?, flush)
}
