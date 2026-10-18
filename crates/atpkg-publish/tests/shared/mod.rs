// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shared types' own tests.

mod pins;
mod quad;
mod triple;

/// A scratch directory removed on drop.
pub fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-publish-{label}-"))
        .tempdir()
        .expect("scratch dir")
}
