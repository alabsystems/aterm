// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durability flushes for atomic file and directory publications.

use aterm_durable::Flush;
use std::fs::File;
use std::path::Path;

/// Flush `file` durably via [`aterm_durable::durable`].
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Finish a write to `file` with `flush` ([`Flush::Durable`] via [`sync_all`] or [`Flush::Ordered`] via [`aterm_durable::ordered`]).
pub(crate) fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}

/// Flush the directory `dir` with [`finish`].
pub(crate) fn sync_dir(dir: &Path, flush: Flush) -> std::io::Result<()> {
    finish(&File::open(dir)?, flush)
}
