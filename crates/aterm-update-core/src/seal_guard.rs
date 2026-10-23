// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared `…/aterm/Updates` root path derivation.

use std::path::PathBuf;

/// Returns the `…/aterm/Updates` root directory, creating it privately if needed.
#[must_use]
pub fn updates_root() -> Option<PathBuf> {
    let root = updates_base()?.join("Updates");
    crate::ensure_private_dir(&root).ok()?;
    Some(root)
}

/// Returns the base directory containing [`updates_root`] without creating it.
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
