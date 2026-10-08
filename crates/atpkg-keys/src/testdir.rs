// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit-test scratch directories, removed when the test ends HOWEVER it ends — atpkg's
//! `testdir::Dir`, which a test-only module there cannot lend to this crate.
//!
//! The directory is an [`aterm_tempfile::TempDir`], so a failing assertion unwinds
//! through its drop and the shared `$TMPDIR` is left as it was found. The name keeps the
//! helper's label and is unique per call, so concurrent runs of the suite never share or
//! wipe each other's tree.
//! Only a BORROW of the guard is a path: moving it into a path-taking call would drop —
//! and delete — the directory at that call, so it does not compile.

use std::ops::Deref;
use std::path::{Path, PathBuf};

use aterm_tempfile::TempDir;

/// A scratch directory: dereferences to its path, removed on drop.
#[derive(Debug)]
pub(crate) struct Dir {
    path: PathBuf,
    _dir: TempDir,
}

/// A fresh scratch directory under the temp dir, named `<prefix><unique>`.
pub(crate) fn dir(prefix: &str) -> Dir {
    let dir = aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create a scratch directory under the temp dir");
    Dir {
        path: dir.path().to_path_buf(),
        _dir: dir,
    }
}

impl Deref for Dir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

/// The borrow, not the guard: see the module doc for why `Dir` itself is not
/// `AsRef<Path>`.
impl AsRef<Path> for &Dir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}
