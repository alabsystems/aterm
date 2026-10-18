// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STORE'S LAYOUT, BY NAME: where under an atpkg prefix a build, its exec root and a
//! seam view live. atpkg's `Layout::build_dir`, `compat::COMPAT_DIR` and
//! `seam::views_root` are these, so the lease ([`crate::lease`]) and the toolchain walk
//! ([`crate::discovery`]) read the store where atpkg lays it.

use std::path::{Path, PathBuf};

/// `<prefix>/store` — the versioned, immutable builds, `store/<program>/<build>`.
pub const STORE_DIR: &str = "store";

/// `<prefix>/compat` — the store's exec roots, one per build
/// (`compat/<program>/<build>`), outside `store/` since nothing is laid inside the
/// content-addressed store.
pub const COMPAT_DIR: &str = "compat";

/// `<prefix>/rustup` — the seam's views, one per rustup toolchain name.
pub const VIEWS_DIR: &str = "rustup";

/// `<prefix>/store/<program>/<build>`.
#[must_use]
pub fn build_dir(prefix: &Path, program: &str, build: u64) -> PathBuf {
    prefix.join(STORE_DIR).join(program).join(build.to_string())
}

/// `<prefix>/rustup/<name>` — the view the seam `name` targets.
#[must_use]
pub fn view_dir(prefix: &Path, name: &str) -> PathBuf {
    prefix.join(VIEWS_DIR).join(name)
}
