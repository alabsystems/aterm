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
    /// Removed as a build cache pass removes a stage ([`crate::build_cache::remove::tree`]): no
    /// one entry a test left unremovable (a folder without its owner's bits, a `deny delete`
    /// ACL) keeps the rest. What that leaves (the remover under test failing) is unlocked and
    /// removed without it ([`unlock`]), so a failed test leaves no scratch a plain remover
    /// cannot remove.
    fn drop(&mut self) {
        if !crate::build_cache::remove::tree(&self.path).whole() {
            unlock(&self.path);
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Give `at` and every entry under it, never through a link, its owner's access back by path,
/// with nothing of the build cache's remover, a folder before what it holds and never by
/// recursion (a tree of any depth unlocks in one frame): each folder its owner's `rwx` and each
/// file its owner's `rw`, then on macOS each one's extended ACL cleared: `/bin/chmod -N`, and
/// `-h -N` on a link (its own ACL, never its target's). `-h` opens what it names, so it refuses an
/// entry that denies its own reading, which plain `-N` clears.
#[cfg(unix)]
fn unlock(at: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut stack = vec![at.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            continue;
        };
        let owner = if meta.is_dir() {
            0o700
        } else if meta.is_file() {
            0o600
        } else {
            0
        };
        if owner != 0 {
            let mode = meta.permissions().mode() | owner;
            let _ = std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode));
        }
        #[cfg(target_os = "macos")]
        {
            use aterm_uds::spawnfd::Stdio;
            let mut chmod = aterm_uds::spawnfd::command("/bin/chmod");
            if meta.file_type().is_symlink() {
                chmod.arg("-h");
            }
            let _ = chmod
                .arg("-N")
                .arg(&at)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        if meta.is_dir() {
            stack.extend(
                std::fs::read_dir(&at)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path()),
            );
        }
    }
}

#[cfg(not(unix))]
const fn unlock(_: &Path) {}

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

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    /// A chain of `levels` folders named `d` under `top`, each at 0o500 (its owner may not
    /// remove what it holds).
    fn sealed_chain(top: &Path, levels: usize) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut deep = top.to_path_buf();
        for _ in 0..levels {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        while deep != top {
            std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o500)).unwrap();
            deep.pop();
        }
    }

    /// THE SCRATCH'S FALLBACK UNLOCKS A TREE OF ANY DEPTH IN ONE FRAME: a chain of 300 folders
    /// each at 0o500 is given its owner's bits back by [`super::unlock`] on a thread of 128 KiB
    /// of stack (one frame a folder runs out of it), and then goes by a plain remover.
    /// NEGATIVE CONTROL: an identical chain refuses `std::fs::remove_dir_all`.
    #[test]
    fn the_fallback_unlocks_a_tree_of_any_depth_in_one_frame() {
        let dir = super::dir("testdir-unlock-deep");
        let (top, control) = (dir.join("top"), dir.join("control"));
        sealed_chain(&top, 300);
        sealed_chain(&control, 300);
        assert!(std::fs::remove_dir_all(&control).is_err());

        let at = top.clone();
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || super::unlock(&at))
            .unwrap()
            .join()
            .unwrap();
        std::fs::remove_dir_all(&top).unwrap();
    }
}
