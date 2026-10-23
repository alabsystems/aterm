// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Atomic durability flush helpers for release journals, records, and artifacts.

use std::fs::File;
use std::path::Path;

/// Flushes an open file or directory to durable storage.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Flushes file contents without a drive-cache barrier under Apple tests.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    atpkg::platform::sync_file_contents(file)
}

/// Flushes directory `dir` to make a rename or new entry durable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    sync_all(&File::open(dir)?)
}
