// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Publish a replacement hard link without reserving an admitted payload filename.

use std::io;
use std::path::Path;

/// Stage in an exclusively created directory using the alias's parent and the helper's
/// shared-root quiet placement. Only that owned directory is cleaned on return; an occupied
/// payload name is never removed to make room.
pub(super) fn replace(primary: &Path, alias: &Path) -> io::Result<()> {
    let parent = alias
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let scratch = aterm_tempfile::Builder::new()
        .prefix(".atpkg-relink-")
        .tempdir_in(parent)?;
    let temporary = scratch.path().join("link");
    std::fs::hard_link(primary, &temporary)?;
    std::fs::rename(&temporary, alias)
}
