// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durability flushes for atomic file publications. Directory writers use
//! [`aterm_durable::Flush::dir_best_effort`] or [`aterm_durable::durable_dir`].

use std::fs::File;

/// Flush `file` durably via [`aterm_durable::durable`].
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Finish a macOS record's write to `file` with `flush` ([`aterm_durable::Flush::Durable`]
/// via [`sync_all`] or [`aterm_durable::Flush::Ordered`] via [`aterm_durable::ordered`]).
#[cfg(target_os = "macos")]
pub(crate) fn finish(file: &File, flush: aterm_durable::Flush) -> std::io::Result<()> {
    match flush {
        aterm_durable::Flush::Durable => sync_all(file),
        aterm_durable::Flush::Ordered => aterm_durable::ordered(file),
    }
}
