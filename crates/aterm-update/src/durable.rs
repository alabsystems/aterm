// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The crate's durability flushes: the barrier every atomic publish takes on its
//! temporary before the rename (`manifest::write_durable`, the Linux lane's records and
//! executables) and on the directory after it (those, and a bundle swap's parents).

use aterm_durable::Flush;
use std::fs::File;
use std::path::Path;

/// Make one open file or directory DURABLE. It is [`aterm_durable::durable`], which on
/// Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache flush, and `fsync(2)` elsewhere (a
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

/// [`finish`] on the directory `dir`: a rename or an exchange in it made durable, or
/// ordered ahead of what follows.
pub(crate) fn sync_dir(dir: &Path, flush: Flush) -> std::io::Result<()> {
    finish(&File::open(dir)?, flush)
}
