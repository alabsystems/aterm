// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit-test scratch directories and store prefixes removed on drop.

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

/// A store prefix that does not exist yet (`<scratch>/prefix`), removed on drop.
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

/// Runs `case` across `items` on up to eight worker threads and re-raises the first panic.
pub(crate) fn each_side_by_side<T: Sync>(items: &[T], case: impl Fn(usize, &T) + Sync) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..items.len().min(8))
            .map(|_| {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        case(i, item);
                    }
                })
            })
            .collect();
        let mut first = None;
        for worker in workers {
            if let Err(panic) = worker.join() {
                first.get_or_insert(panic);
            }
        }
        if let Some(panic) = first {
            std::panic::resume_unwind(panic);
        }
    });
}
