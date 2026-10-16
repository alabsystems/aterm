// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where the app updater's `…/aterm/Updates` root lives: one derivation shared by
//! `aterm-update`'s staging layout and the tests and tools that read it. The module name is
//! historical.

use std::path::PathBuf;

/// The `…/aterm/Updates` root: under the HOME-keyed Application Support base, which a
/// development build may relocate with the `ATERM_UPDATE_ROOT` seam (a shipped binary does not
/// read it). `None` when HOME is unset or the directory cannot be made private.
#[must_use]
pub fn updates_root() -> Option<PathBuf> {
    let base = match aterm_types::dev_seam!("ATERM_UPDATE_ROOT") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("aterm")
        }
    };
    let root = base.join("Updates");
    crate::ensure_private_dir(&root).ok()?;
    Some(root)
}
