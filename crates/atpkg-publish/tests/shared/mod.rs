// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shared types' own tests.

mod pins;
mod quad;
mod triple;

/// The pack's compressor, the `zstd` CLI on `PATH` as the producer runs it
/// (`tarwrite::compressor_on_path`), or the calling test ends COULD NOT RUN saying what
/// removes the need (its `TOOLS` row's remedy): a host without zstd did not run the test,
/// which is no verdict on the producer (docs/DESIGN-init-system-2026-10-20.md §8, rule 5).
///
/// # Panics
///
/// Without it: that is the COULD NOT RUN.
#[track_caller]
pub fn need_compressor() {
    if let Err(why) = atpkg_publish::artifact::tarwrite::compressor_on_path() {
        aterm_uds::host::could_not_run(format!(
            "{why}: {}",
            aterm_uds::host::tool("zstd").remedy()
        ));
    }
}

/// A scratch directory removed on drop.
pub fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-publish-{label}-"))
        .tempdir()
        .expect("scratch dir")
}

/// One entry of a [`Tree`].
enum Node {
    Dir,
    File { bytes: Vec<u8>, mode: u32 },
}

/// A directory tree held in memory — each directory, and each file's bytes and mode, by its
/// path under the root — so a fixture whose making costs processes (a git repository) is
/// made once per process and laid down per test.
///
/// It remembers the root it was read from: a made tree can name it (git writes a clone's
/// source path into the clone's config, reflogs and `FETCH_HEAD`), and [`Tree::lay`] moves
/// that name to the root it lays the tree at.
pub struct Tree {
    root: Vec<u8>,
    nodes: Vec<(std::path::PathBuf, Node)>,
}

impl Tree {
    /// Everything under `root`.
    ///
    /// # Panics
    ///
    /// When `root` cannot be read.
    #[must_use]
    pub fn read(root: &std::path::Path) -> Self {
        fn walk(root: &std::path::Path, rel: &std::path::Path, out: &mut Tree) {
            use std::os::unix::fs::PermissionsExt as _;
            for e in std::fs::read_dir(root.join(rel)).expect("read a fixture dir") {
                let e = e.expect("a fixture entry");
                let rel = rel.join(e.file_name());
                let meta = e.metadata().expect("stat a fixture entry");
                // `metadata` here does not follow a link. Read as a file, a link would be laid
                // down as a copy of what it names (a world unlike the one made), and a fifo
                // would hang the read.
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

    /// The tree under `root`, each file's mention of the root it was read from moved to
    /// `root`.
    ///
    /// # Panics
    ///
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

/// `bytes` with every `from` replaced by `to`.
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

/// What `make` leaves in `dir` (which must hold nothing yet), made once per process for each
/// `key` — by the first caller, in its own `dir` — and laid down at `dir` for every caller
/// after; with what `make` returned.
///
/// # Panics
///
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
