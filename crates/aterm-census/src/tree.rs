// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census's reads of the checkout it walks.

use std::io;
use std::path::{Path, PathBuf};

/// The file at `path`, as UTF-8 (`std::fs::read_to_string`).
pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::read) {
        return answer;
    }
    std::fs::read_to_string(path)
}

/// Whether `path` is a directory (`Path::is_dir`).
pub(crate) fn is_dir(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::is_dir) {
        return answer;
    }
    path.is_dir()
}

/// Whether `path` is a file (`Path::is_file`).
pub(crate) fn is_file(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::is_file) {
        return answer;
    }
    path.is_file()
}

/// Whether anything is at `path` (`Path::exists`).
pub(crate) fn exists(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, |files, rel| files.is_file(rel) || files.is_dir(rel))
    {
        return answer;
    }
    path.exists()
}

/// List the entries of the directory at `path`.
pub(crate) fn read_dir(path: &Path) -> io::Result<Vec<io::Result<PathBuf>>> {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, |files, rel| files.read_dir(path, rel)) {
        return answer;
    }
    Ok(std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect())
}

#[cfg(test)]
pub(crate) mod memory {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// One tree's files, by repo-relative path, and its faults.
    #[derive(Default)]
    pub(super) struct Files {
        held: BTreeMap<PathBuf, String>,
        /// The entries its listings cannot read ([`Tree::unreadable`]).
        unreadable: BTreeSet<PathBuf>,
        /// The directories it cannot list ([`Tree::unlistable`]).
        unlistable: BTreeSet<PathBuf>,
    }

    impl Files {
        pub(super) fn read(&self, rel: &Path) -> io::Result<String> {
            match self.held.get(rel) {
                Some(text) => Ok(text.clone()),
                None if self.is_dir(rel) => Err(io::ErrorKind::IsADirectory.into()),
                None => Err(self.missing(rel)),
            }
        }

        fn missing(&self, rel: &Path) -> io::Error {
            if rel.ancestors().skip(1).any(|up| self.held.contains_key(up)) {
                io::ErrorKind::NotADirectory.into()
            } else {
                io::ErrorKind::NotFound.into()
            }
        }

        pub(super) fn is_file(&self, rel: &Path) -> bool {
            self.held.contains_key(rel)
        }

        /// The root, and every proper ancestor of a file.
        pub(super) fn is_dir(&self, rel: &Path) -> bool {
            rel.as_os_str().is_empty()
                || self
                    .held
                    .keys()
                    .any(|file| file != rel && file.starts_with(rel))
        }

        /// The names one level under `rel`, joined onto `path` (the same
        /// directory, as the caller spelled it); an unreadable entry
        /// ([`Tree::unreadable`]) is an error in its place, and an unlistable
        /// directory ([`Tree::unlistable`]) is refused whole.
        pub(super) fn read_dir(
            &self,
            path: &Path,
            rel: &Path,
        ) -> io::Result<Vec<io::Result<PathBuf>>> {
            if self.is_file(rel) {
                return Err(io::ErrorKind::NotADirectory.into());
            }
            if !self.is_dir(rel) {
                return Err(self.missing(rel));
            }
            if self.unlistable.contains(rel) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let names: BTreeSet<_> = self
                .held
                .keys()
                .filter_map(|file| file.strip_prefix(rel).ok())
                .filter_map(|below| below.components().next())
                .map(|name| name.as_os_str().to_owned())
                .collect();
            Ok(names
                .into_iter()
                .map(|name| {
                    if self.unreadable.contains(&rel.join(&name)) {
                        Err(io::Error::other("an entry this listing cannot read"))
                    } else {
                        Ok(path.join(name))
                    }
                })
                .collect())
        }
    }

    static TREES: Mutex<BTreeMap<PathBuf, Files>> = Mutex::new(BTreeMap::new());

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn trees() -> MutexGuard<'static, BTreeMap<PathBuf, Files>> {
        TREES.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `ask` of the tree whose root `path` is under, with `path` relative to
    /// it; `None` when no tree holds `path` (it is on the disk).
    pub(super) fn answer<T>(path: &Path, ask: impl FnOnce(&Files, &Path) -> T) -> Option<T> {
        let trees = trees();
        let (root, files) = trees.iter().find(|(root, _)| path.starts_with(root))?;
        let rel = path.strip_prefix(root).ok()?;
        Some(ask(files, rel))
    }

    /// A checkout held in memory while this value lives.
    pub(crate) struct Tree {
        root: PathBuf,
    }

    impl Tree {
        /// Create an in-memory tree of `files` labeled `name`.
        pub(crate) fn new(name: &str, files: &[(String, String)]) -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = Path::new("/aterm-census-memory").join(format!("{name}-{n}"));
            let held = files
                .iter()
                .map(|(rel, text)| (PathBuf::from(rel), text.clone()))
                .collect();
            trees().insert(
                root.clone(),
                Files {
                    held,
                    ..Files::default()
                },
            );
            Self { root }
        }

        /// Replace (or add) the file at `rel`.
        pub(crate) fn write(&self, rel: &str, contents: &str) {
            if let Some(files) = trees().get_mut(&self.root) {
                files.held.insert(PathBuf::from(rel), contents.to_string());
            }
        }

        /// Remove the file at `rel`.
        pub(crate) fn remove(&self, rel: &str) {
            if let Some(files) = trees().get_mut(&self.root) {
                files.held.remove(Path::new(rel));
            }
        }

        /// Mark the entry at `rel` unreadable during directory listing.
        pub(crate) fn unreadable(&self, rel: &str) {
            if let Some(files) = trees().get_mut(&self.root) {
                files.unreadable.insert(PathBuf::from(rel));
            }
        }

        /// Mark the directory at `rel` unlistable (`PermissionDenied`).
        pub(crate) fn unlistable(&self, rel: &str) {
            if let Some(files) = trees().get_mut(&self.root) {
                files.unlistable.insert(PathBuf::from(rel));
            }
        }
    }

    impl std::ops::Deref for Tree {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            trees().remove(&self.root);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::Tree;
        use std::path::{Path, PathBuf};

        fn answers(root: &Path, rels: &[&str]) -> Vec<String> {
            let rel = |p: &PathBuf| p.strip_prefix(root).map(Path::to_path_buf);
            rels.iter()
                .map(|r| {
                    let path = root.join(r);
                    let listing = crate::tree::read_dir(&path)
                        .and_then(|entries| {
                            entries.into_iter().collect::<std::io::Result<Vec<_>>>()
                        })
                        .map(|mut entries| {
                            entries.sort();
                            entries.iter().map(rel).collect::<Vec<_>>()
                        });
                    format!(
                        "{r}: read {:?} dir {} file {} exists {} list {:?}",
                        crate::tree::read_to_string(&path).map_err(|e| e.kind()),
                        crate::tree::is_dir(&path),
                        crate::tree::is_file(&path),
                        crate::tree::exists(&path),
                        listing.map_err(|e| e.kind()),
                    )
                })
                .collect()
        }

        #[test]
        fn a_memory_tree_answers_as_the_same_files_on_disk() {
            let files = [
                ("Cargo.toml", "[workspace]\n"),
                ("crates/a/src/lib.rs", "// a\n"),
                ("crates/a/src/m/x.rs", "// x\n"),
            ];
            let queries = [
                "",
                "Cargo.toml",
                "crates",
                "crates/a",
                "crates/a/src",
                "crates/a/src/lib.rs",
                "crates/a/src/m",
                "crates/a/src/m/x.rs",
                "crates/b",
                "crates/a/src/li",
                "crates/a/src/lib.rs/x",
            ];
            let disk =
                std::env::temp_dir().join(format!("aterm-census-tree-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&disk);
            let write = |rel: &str, text: &str| {
                let path = disk.join(rel);
                std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
                std::fs::write(path, text).expect("write");
            };
            for (rel, text) in files {
                write(rel, text);
            }
            let tree = Tree::new(
                "shape",
                &files.map(|(rel, text)| (rel.to_string(), text.to_string())),
            );
            let on_disk = answers(&disk, &queries);
            assert_eq!(answers(&tree, &queries), on_disk);
            assert!(on_disk.iter().any(|a| a.contains("Ok(\"// a\\n\")")));
            assert!(on_disk.iter().any(|a| a.contains("IsADirectory")));
            assert!(on_disk.iter().any(|a| a.contains("NotADirectory")));
            assert!(on_disk.iter().any(|a| a.contains("NotFound")));

            write("crates/b/src/lib.rs", "// b\n");
            tree.write("crates/b/src/lib.rs", "// b\n");
            std::fs::remove_file(disk.join("crates/a/src/m/x.rs")).expect("rm");
            std::fs::remove_dir(disk.join("crates/a/src/m")).expect("rmdir");
            tree.remove("crates/a/src/m/x.rs");
            let queries = [queries.as_slice(), &["crates/b/src", "crates/b/src/lib.rs"]].concat();
            assert_eq!(answers(&tree, &queries), answers(&disk, &queries));
            let _ = std::fs::remove_dir_all(&disk);

            let file = tree.join("Cargo.toml");
            assert!(crate::tree::is_file(&file));
            drop(tree);
            assert!(!crate::tree::exists(&file));
        }
    }
}
