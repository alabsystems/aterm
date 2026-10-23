// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-memory repository [`Template`] and [`Tree`] fixture built once per process.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One entry of a [`Tree`].
enum Node {
    Dir,
    File { bytes: Vec<u8>, mode: u32 },
}

/// A directory tree held in memory.
pub(crate) struct Tree(Vec<(PathBuf, Node)>);

/// A fixture of repositories ([`Tree`]).
pub(crate) struct Template<T> {
    built: OnceLock<(PathBuf, Tree, T)>,
}

impl<T: Clone> Template<T> {
    pub(crate) const fn new() -> Self {
        Self {
            built: OnceLock::new(),
        }
    }

    /// The fixture under `root` (an empty directory), and what `build` returned the
    /// once it built it in a scratch dir of its own (the commit ids a test reads, say).
    pub(crate) fn lay(&self, root: &Path, build: impl FnOnce(&Path) -> T) -> T {
        let (from, tree, value) = self.built.get_or_init(|| {
            let dir = aterm_tempfile::Builder::new()
                .prefix("aterm-release-template-")
                .tempdir()
                .unwrap();
            let from = dir.path().to_path_buf();
            let value = build(&from);
            (from.clone(), Tree::read(&from), value)
        });
        tree.lay(from, root);
        value.clone()
    }
}

impl Tree {
    pub(crate) fn read(root: &Path) -> Self {
        fn walk(root: &Path, rel: &Path, out: &mut Tree) {
            use std::os::unix::fs::PermissionsExt as _;
            for e in fs::read_dir(root.join(rel)).unwrap() {
                let e = e.unwrap();
                let rel = rel.join(e.file_name());
                let meta = e.metadata().unwrap();
                if meta.is_dir() {
                    out.0.push((rel.clone(), Node::Dir));
                    walk(root, &rel, out);
                } else {
                    let bytes = fs::read(e.path()).unwrap();
                    let mode = meta.permissions().mode();
                    out.0.push((rel, Node::File { bytes, mode }));
                }
            }
        }
        let mut tree = Self(Vec::new());
        walk(root, Path::new(""), &mut tree);
        tree
    }

    /// Write the tree under `root`, rewriting `from` in git `config` files.
    pub(crate) fn lay(&self, from: &Path, root: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let (from, to) = (from.to_string_lossy(), root.to_string_lossy());
        for (rel, node) in &self.0 {
            let path = root.join(rel);
            let Node::File { bytes, mode } = node else {
                fs::create_dir_all(&path).unwrap();
                continue;
            };
            if rel.file_name().is_some_and(|n| n == "config") {
                let text = String::from_utf8_lossy(bytes).replace(&*from, &to);
                fs::write(&path, text).unwrap();
            } else {
                fs::write(&path, bytes).unwrap();
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
        }
    }
}
