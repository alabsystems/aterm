// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's flushes, in one place, for it and for aterm-gui.

pub use aterm_durable::Flush;
use std::fs::File;

/// Make one open file or directory DURABLE.
#[inline]
pub fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Finish a write to `file` with `flush`.
/// # Errors
/// The flush's own error.
#[inline]
pub fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}
