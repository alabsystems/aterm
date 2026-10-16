// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit-test scratch directories, removed when the test ends however it ends.
//!
//! Each is an [`aterm_tempfile::TempDir`] (mode `0700`, the shape the store's ownership
//! checks require), so a failing assertion unwinds through its drop. The name keeps the
//! helper's label and is unique per call, so concurrent tests and suite runs never share
//! a tree. Bound first, a guard drops last, after any runner or host claim whose own drop
//! still writes into the prefix. A removal that fails (a test took a write bit away and
//! failed before restoring it) gives the tree's directories their owner bits back and
//! tries once more.
//!
//! Only a borrow of a guard is a path (`&dir`, `&*dir`, `dir.join(…)`): moving one into a
//! path-taking call would delete the directory there, so it does not compile. An unbound
//! guard is not caught: `scratch("x").canonicalize()` deletes the directory at the end of
//! that statement.

use std::ops::Deref;
use std::path::{Path, PathBuf};

use aterm_tempfile::TempDir;

use crate::store::Layout;

/// A scratch directory: dereferences to its path, removed on drop.
#[derive(Debug)]
pub(crate) struct Dir {
    path: PathBuf,
    _dir: TempDir,
}

/// A fresh scratch directory under the temp dir, named `<prefix><unique>`.
pub(crate) fn dir(prefix: &str) -> Dir {
    dir_in(&std::env::temp_dir(), prefix)
}

/// [`dir`] under `parent` instead of the temp dir.
pub(crate) fn dir_in(parent: &Path, prefix: &str) -> Dir {
    let dir = aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .expect("create a scratch directory");
    Dir {
        path: dir.path().to_path_buf(),
        _dir: dir,
    }
}

impl Drop for Dir {
    /// Runs before the `TempDir`'s own removal, which then finds nothing left.
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.path).is_err() {
            restore_owner_access(&self.path);
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Give `dir` and every directory under it (never through a symlink) its owner's
/// `rwx` back, so `remove_dir_all` can empty them.
#[cfg(unix)]
fn restore_owner_access(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode() | 0o700;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode));
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        restore_owner_access(&entry.path());
    }
}

#[cfg(not(unix))]
fn restore_owner_access(_: &Path) {}

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

/// A scratch store prefix: dereferences to its [`Layout`], removed on drop.
#[derive(Debug)]
pub(crate) struct ScratchLayout {
    layout: Layout,
    _dir: Dir,
}

/// A fresh scratch store prefix under the temp dir, named `<prefix><unique>`.
pub(crate) fn layout(prefix: &str) -> ScratchLayout {
    let dir = dir(prefix);
    ScratchLayout {
        layout: Layout {
            prefix: dir.to_path_buf(),
        },
        _dir: dir,
    }
}

/// A store prefix that does not exist yet (`<scratch>/prefix`), inside a fresh scratch
/// directory removed on drop.
pub(crate) fn absent_layout(prefix: &str) -> ScratchLayout {
    let dir = dir(prefix);
    ScratchLayout {
        layout: Layout {
            prefix: dir.join("prefix"),
        },
        _dir: dir,
    }
}

impl Deref for ScratchLayout {
    type Target = Layout;
    fn deref(&self) -> &Layout {
        &self.layout
    }
}
