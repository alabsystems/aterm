// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test's scratch folder that a journal's writer admits, removed when it drops: a temporary
//! folder, which Unix makes `0700`; on Windows, where a temporary folder takes its access list
//! from the one it is in, a private folder made in one ([`crate::fsx::create_private_dir_new`]:
//! the user's under a protected list of the user and SYSTEM from its creation), as the writer
//! requires (`aterm_journal`'s admission refuses a folder whose list takes entries from its
//! own).

use std::path::{Path, PathBuf};

/// A scratch folder a journal can live in.
pub(crate) struct PrivateScratch {
    _root: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl PrivateScratch {
    /// The folder.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// A new [`PrivateScratch`].
pub(crate) fn tempdir() -> PrivateScratch {
    let root = aterm_tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let path = root.path().to_path_buf();
    #[cfg(windows)]
    let path = {
        let path = root.path().join("private");
        crate::fsx::create_private_dir_new(&path).unwrap();
        path
    };
    PrivateScratch { _root: root, path }
}
