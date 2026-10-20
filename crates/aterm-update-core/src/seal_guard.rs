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
    let root = updates_base()?.join("Updates");
    crate::ensure_private_dir(&root).ok()?;
    Some(root)
}

/// The folder [`updates_root`]'s `Updates` sits in, resolved, never made: the
/// `ATERM_UPDATE_ROOT` seam; else, in a Cargo test binary, its own root's `state`
/// (`aterm_types::dirs::test_root`), so a test that names no root never makes or
/// reads the person's ledger; else `$HOME/Library/Application Support/aterm`.
/// `None` when HOME is unset there.
#[must_use]
pub fn updates_base() -> Option<PathBuf> {
    if let Some(root) = aterm_types::dev_seam!("ATERM_UPDATE_ROOT").filter(|root| !root.is_empty())
    {
        return Some(PathBuf::from(root));
    }
    if let Some(root) = aterm_types::dirs::test_root() {
        return Some(root.join("state"));
    }
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("aterm"),
    )
}
