// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's flushes, in one place: the harness's atomic writes
//! ([`crate::harness::upgrade_models::write_atomic_mode`]) and its ledger cut
//! ([`crate::harness::cli::bound_ledger`]). Each finishes through aterm-durable:
//! [`sync_all`] is its DURABLE arm; a site that needs only its order kept calls
//! [`aterm_durable::ordered`] itself, or [`finish`] with [`Flush::Ordered`].

use aterm_durable::Flush;
use std::fs::File;

/// Make one open file or directory DURABLE: [`aterm_durable::durable`], which on
/// Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache flush, and `fsync(2)`
/// elsewhere (a plain `fsync(2)` in a test or debug build on Apple targets, as every
/// durable flush there).
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Finish a write to `file` with `flush`: [`Flush::Durable`] through [`sync_all`],
/// [`Flush::Ordered`] through [`aterm_durable::ordered`].
pub(crate) fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}
