// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! File flush helpers backed by [`aterm_durable`]; a directory takes [`Flush::dir_best_effort`].

use aterm_durable::Flush;
use std::fs::File;

/// Make one open file or directory durable: [`aterm_durable::durable`], which is
/// `fcntl(F_FULLFSYNC)` (a drive-cache flush) on Apple targets and `fsync(2)` elsewhere (a
/// plain `fsync(2)` in a test or debug build on Apple targets, as every durable flush there).
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
