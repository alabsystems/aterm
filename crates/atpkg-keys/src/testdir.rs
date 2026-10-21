// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit-test scratch directories, removed when the test ends however it ends: atpkg's
//! `testdir::Dir`, which a test-only module there cannot lend to this crate.
//!
//! Only a borrow of the guard is a path: moving the guard into a path-taking call would drop,
//! and delete, the directory at that call, so it does not compile.

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

/// The borrow, not the guard (see the module doc).
impl AsRef<Path> for &Dir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}
