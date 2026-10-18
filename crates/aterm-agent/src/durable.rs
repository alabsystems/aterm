// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's flushes, in one place: the harness's atomic writes
//! ([`crate::harness::upgrade_models::write_atomic_mode`]) and its ledger cut
//! ([`crate::harness::cli::bound_ledger`]). Each finishes through aterm-durable:
//! [`sync_all`] is its DURABLE arm, kept here for this crate's unit-test arm below; a
//! site that needs only its order kept calls [`aterm_durable::ordered`] itself, or
//! [`finish`] with [`Flush::Ordered`].

use aterm_durable::Flush;
use std::fs::File;

/// Make one open file or directory DURABLE: [`aterm_durable::durable`], which on
/// Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache flush, and `fsync(2)`
/// elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// [`sync_all`] under this crate's own unit tests on Apple targets: a plain `fsync(2)`
/// ([`aterm_durable::seam::durable_fsync`], which the counting seam records as
/// durable). It is still a real flush, made at the same point and in the same order,
/// so a test sees the same files, renames and errors. What it drops is the drive-cache
/// barrier. No test can observe that barrier, because it only orders the disk against
/// a power loss. It also stalls the whole VOLUME while it runs, so every test binary
/// running beside this one waits behind it. Measured: one `aterm-agent --lib` run made
/// 2,561 `F_FULLFSYNC`s (17.7 s of flushing); with these arms it makes 2, and the run
/// takes about 4 s less alone and slows its neighbours less. Off Apple targets
/// `sync_all` already is `fsync(2)`. Integration tests, shipped builds and dependent
/// crates keep the full barrier.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::seam::durable_fsync(file)
}

/// Finish a write to `file` with `flush`: [`Flush::Durable`] through [`sync_all`] (and
/// its test arm), [`Flush::Ordered`] through [`aterm_durable::ordered`].
pub(crate) fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}
