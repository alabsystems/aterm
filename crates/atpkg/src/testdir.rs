// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit-test scratch directories, removed when the test ends HOWEVER it ends.
//!
//! A test module's scratch helper (`layout`, `scratch`, `tmp`, …) returns one of these
//! guards rather than a bare path it made under `temp_dir()`. The directory is an
//! [`aterm_tempfile::TempDir`], so a failing assertion unwinds through its drop: the
//! suite leaves the shared `$TMPDIR` as it found it, where a trailing
//! `remove_dir_all` line ran only when every assertion before it passed — and a test
//! that forgot the line leaked on every run. The name keeps the helper's label
//! (`atpkg-doctor-<label>-<pid>_<rand>_<n>`) and is unique per call, so two tests, or
//! two concurrent runs of the suite, never share or wipe each other's tree. It is
//! created mode `0700` (TempDir's own contract), the private shape the store's
//! ownership checks require.
//!
//! Bound first, a guard also drops LAST: after a runner or a host claim the test made
//! later, whose own drop can still write into the prefix (measured 2026-09-30: the vendor
//! head watch's tests recreated their removed prefix that way). A test that takes a
//! directory's write bit away (the permission tests here do, to make a write fail) and
//! then fails before giving it back leaves a tree `remove_dir_all` cannot empty, so a
//! removal that fails gives the tree's directories their owner bits back and tries once
//! more.
//!
//! Only a BORROW of a guard is a path (`&dir` to an `AsRef<Path>` parameter, `&*dir`,
//! `dir.join(…)` through `Deref`). Moving one into a path-taking call would drop —
//! and delete — the directory at that call, so it does not compile. What the compiler
//! cannot catch is an unbound guard: `scratch("x").canonicalize()` deletes the directory
//! at the end of that statement, so bind the guard and borrow from the binding.

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

/// A store prefix that does not exist yet — `<scratch>/prefix`, for the tests about
/// what creates it — inside a fresh scratch directory that is removed on drop, with
/// anything a test lays beside the prefix.
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
