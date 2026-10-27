// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shared types' own tests.

mod git_history;
mod pins;
mod quad;
mod triple;

/// A scratch directory removed on drop.
pub fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-publish-{label}-"))
        .tempdir()
        .expect("scratch dir")
}

enum Node {
    Dir,
    File { bytes: Vec<u8>, mode: u32 },
}

/// An in-memory directory tree snapshot that rewrites its source root path when laid down.
pub struct Tree {
    root: Vec<u8>,
    nodes: Vec<(std::path::PathBuf, Node)>,
}

impl Tree {
    /// Everything under `root`.
    /// # Panics
    /// When `root` cannot be read.
    #[must_use]
    pub fn read(root: &std::path::Path) -> Self {
        fn walk(root: &std::path::Path, rel: &std::path::Path, out: &mut Tree) {
            use std::os::unix::fs::PermissionsExt as _;
            for e in std::fs::read_dir(root.join(rel)).expect("read a fixture dir") {
                let e = e.expect("a fixture entry");
                let rel = rel.join(e.file_name());
                let meta = e.metadata().expect("stat a fixture entry");
                assert!(
                    meta.is_dir() || meta.is_file(),
                    "{} is neither a file nor a directory: a made fixture holds only those",
                    e.path().display()
                );
                if meta.is_dir() {
                    out.nodes.push((rel.clone(), Node::Dir));
                    walk(root, &rel, out);
                } else {
                    let bytes = std::fs::read(e.path()).expect("read a fixture file");
                    let mode = meta.permissions().mode();
                    out.nodes.push((rel, Node::File { bytes, mode }));
                }
            }
        }
        use std::os::unix::ffi::OsStrExt as _;
        let mut tree = Self {
            root: root.as_os_str().as_bytes().to_vec(),
            nodes: Vec::new(),
        };
        walk(root, std::path::Path::new(""), &mut tree);
        tree
    }

    /// Writes the tree under `root`, rewriting mentions of the source root path to `root`.
    /// # Panics
    /// When it cannot be written.
    pub fn lay(&self, root: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(root).expect("mkdir a fixture root");
        let to = root.as_os_str().as_bytes();
        for (rel, node) in &self.nodes {
            let path = root.join(rel);
            match node {
                Node::Dir => std::fs::create_dir_all(&path).expect("mkdir a fixture dir"),
                Node::File { bytes, mode } => {
                    std::fs::write(&path, moved(bytes, &self.root, to))
                        .expect("write a fixture file");
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode))
                        .expect("chmod a fixture file");
                }
            }
        }
    }
}

fn moved<'a>(bytes: &'a [u8], from: &[u8], to: &[u8]) -> std::borrow::Cow<'a, [u8]> {
    if from.is_empty() || !bytes.windows(from.len()).any(|w| w == from) {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) = rest.windows(from.len()).position(|w| w == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    std::borrow::Cow::Owned(out)
}

/// What [`made_once`] made, by key: each tree with what its making returned.
pub type Made<K, T> =
    std::sync::Mutex<std::collections::BTreeMap<K, std::sync::Arc<std::sync::OnceLock<(Tree, T)>>>>;

/// Runs `make` once per process for `key` and lays the resulting tree into `dir`.
/// # Panics
/// When `dir` holds anything, or the made tree cannot be read or laid down.
pub fn made_once<K: Ord + Clone, T: Clone>(
    cache: &'static Made<K, T>,
    key: &K,
    dir: &std::path::Path,
    make: impl FnOnce(&std::path::Path) -> T,
) -> T {
    let empty = match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    };
    assert!(
        empty,
        "{} must hold nothing before a made fixture is laid there",
        dir.display()
    );
    let made = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key.clone())
        .or_default()
        .clone();
    let mut made_here = false;
    let (tree, value) = made.get_or_init(|| {
        made_here = true;
        let value = make(dir);
        (Tree::read(dir), value)
    });
    if !made_here {
        tree.lay(dir);
    }
    value.clone()
}
