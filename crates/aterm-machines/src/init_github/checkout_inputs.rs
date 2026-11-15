// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local checkout facts, derived solely from the selected private inventory.
//! These facts request another native read; they never establish Git readiness.

use super::{Asked, asked, inventory, profile, repos};
use crate::model::User;
use crate::port::System;
use aterm_secrets::converge::MarkMoved;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};

const POINTER_BYTES: usize = 4096;

#[derive(Clone, PartialEq, Eq)]
struct Shape {
    uid: u32,
    gid: u32,
    mode: u32,
    directory: bool,
    link: bool,
}

impl From<crate::port::Stat> for Shape {
    fn from(stat: crate::port::Stat) -> Self {
        Self {
            uid: stat.uid,
            gid: stat.gid,
            mode: stat.mode,
            directory: stat.dir,
            link: stat.symlink,
        }
    }
}

/// Cached-scope metadata for the ordinary subscriber's existing input observation lane.
/// It contains no repository content or credential values.
#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryCheckoutMetadata {
    shapes: Vec<(PathBuf, Result<Shape, io::ErrorKind>)>,
    versions: Vec<(PathBuf, Result<[u8; 32], io::ErrorKind>)>,
}

impl std::fmt::Debug for RepositoryCheckoutMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryCheckoutMetadata")
            .finish_non_exhaustive()
    }
}

/// Each watched path's class: which kind of Git metadata it is, to say what moved
/// ([`Checkouts::moved`]). It is no part of what a capture observed, so it takes no part in any
/// comparison: two captures that differ only here are equal.
#[derive(Clone, Default)]
struct Classes(BTreeMap<PathBuf, MarkMoved>);

impl PartialEq for Classes {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Classes {}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Checkouts {
    home: PathBuf,
    uid: u32,
    paths: BTreeMap<PathBuf, bool>,
    classes: Classes,
    metadata: RepositoryCheckoutMetadata,
}

impl Checkouts {
    pub(super) fn capture(sys: &dyn System, user: &User) -> Self {
        let mut scope = Self {
            home: PathBuf::from(&user.home),
            uid: user.uid,
            paths: BTreeMap::new(),
            classes: Classes::default(),
            metadata: RepositoryCheckoutMetadata {
                shapes: Vec::new(),
                versions: Vec::new(),
            },
        };
        if matches!(
            profile(sys, user).and_then(|p| asked(p.and_then(|p| p.github))),
            Ok(Asked::Default)
        ) && let Ok(listed) =
            inventory::Inventory::read(sys, user).map(|i| i.into_repositories())
        {
            for repo in listed {
                if repos::HOME_FOLDERS
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&repo.name))
                {
                    continue;
                }
                let root = scope.home.join(repo.name);
                let admin = root.join(".git");
                if !repo.required_submodules.is_empty() {
                    scope.insert(root.join(".gitmodules"), true, MarkMoved::GITMODULES);
                }
                scope.checkout(sys, &root, &admin, !repo.required_submodules.is_empty());
                for relative in repo.required_submodules {
                    scope.checkout(sys, &root.join(relative), &admin, true);
                }
            }
        }
        scope.metadata = scope.observe(sys);
        scope
    }

    /// Watch `path`, and its version where `version`, as a path of `class`: a path keeps the
    /// first class it was given, so a checkout's `.git` is `git-dir` whether or not it is also
    /// its administrative directory.
    fn insert(&mut self, path: PathBuf, version: bool, class: MarkMoved) {
        self.classes.0.entry(path.clone()).or_insert(class);
        // The private manifest is bounded to one MiB and each repository to 64 declared
        // dependencies. Never silently drop a selected destination to meet a watch limit.
        self.paths
            .entry(path)
            .and_modify(|v| *v |= version)
            .or_insert(version);
    }

    fn checkout(&mut self, sys: &dyn System, root: &Path, allowed_admin: &Path, objects: bool) {
        self.insert(root.to_path_buf(), false, MarkMoved::ROOT);
        let marker = root.join(".git");
        self.insert(marker.clone(), true, MarkMoved::GIT_DIR);
        if !self.admitted(sys, root, true) {
            return;
        }
        let Ok(metadata) = sys.scoped_path_metadata(&marker) else {
            return;
        };
        let stat = metadata.stat;
        if stat.uid != self.uid || stat.symlink {
            return;
        }
        let admin = if stat.dir {
            marker
        } else {
            let Some(pointer) = self.line(sys, &marker) else {
                return;
            };
            let Some(pointer) = pointer.strip_prefix("gitdir: ") else {
                return;
            };
            let Some(target) = normalize(root, pointer) else {
                return;
            };
            // A gitfile cannot enlist an arbitrary directory in background observation.
            if !target.starts_with(allowed_admin) {
                return;
            }
            target
        };
        self.admin(sys, &admin, objects);
        let common = admin.join("commondir");
        if self.admitted(sys, &admin, true)
            && let Some(pointer) = self.line(sys, &common)
            && let Some(target) = normalize(&admin, &pointer)
            && target.starts_with(allowed_admin)
        {
            self.admin(sys, &target, objects);
        }
    }

    fn admin(&mut self, sys: &dyn System, admin: &Path, objects: bool) {
        self.insert(admin.to_path_buf(), true, MarkMoved::ADMIN_DIR);
        for (name, class) in [
            ("config", MarkMoved::CONFIG),
            ("config.worktree", MarkMoved::CONFIG_WORKTREE),
            ("HEAD", MarkMoved::HEAD),
            ("index", MarkMoved::INDEX),
            ("packed-refs", MarkMoved::PACKED_REFS),
            ("shallow", MarkMoved::SHALLOW),
            ("commondir", MarkMoved::POINTER),
            ("gitdir", MarkMoved::POINTER),
            ("refs", MarkMoved::REFS_DIR),
            ("refs/heads", MarkMoved::REFS_HEADS_DIR),
        ] {
            self.insert(admin.join(name), true, class);
        }
        if !self.admitted(sys, admin, true) {
            return;
        }
        if let Some(head) = self.line(sys, &admin.join("HEAD"))
            && let Some(reference) = head.strip_prefix("ref: ")
            && reference.starts_with("refs/")
            && let Some(path) = normalize(admin, reference)
            && path.starts_with(admin.join("refs"))
        {
            self.insert(path, true, MarkMoved::CURRENT_REF);
        }
        if objects {
            let object_root = admin.join("objects");
            for (path, class) in [
                (object_root.clone(), MarkMoved::OBJECTS_DIR),
                (object_root.join("pack"), MarkMoved::OBJECTS_DIR),
                (object_root.join("info"), MarkMoved::OBJECTS_DIR),
                (object_root.join("info/alternates"), MarkMoved::ALTERNATES),
            ] {
                self.insert(path, true, class);
            }
            if self.admitted(sys, &object_root, true) {
                let names: Vec<_> = (0..=255).map(|value| format!("{value:02x}")).collect();
                if let Ok(names) = sys.scoped_child_directories(&object_root, self.uid, &names) {
                    for name in names {
                        self.insert(object_root.join(name), true, MarkMoved::OBJECTS_FANOUT);
                    }
                }
            }
        }
    }

    fn line(&self, sys: &dyn System, path: &Path) -> Option<String> {
        if !self.admitted(sys, path.parent()?, true) {
            return None;
        }
        let bytes = sys
            .read_owned_file_scoped(path, self.uid, POINTER_BYTES, false)
            .ok()?;
        let text = std::str::from_utf8(&bytes)
            .ok()?
            .trim_end_matches(['\r', '\n']);
        (!text.is_empty() && !text.contains(['\r', '\n', '\0'])).then(|| text.to_owned())
    }

    fn admitted(&self, sys: &dyn System, path: &Path, directory: bool) -> bool {
        let Ok(tail) = path.strip_prefix(&self.home) else {
            return false;
        };
        let mut at = self.home.clone();
        let valid = |path: &Path, directory: bool| {
            sys.scoped_path_metadata(path).is_ok_and(|m| {
                let s = m.stat;
                s.uid == self.uid && !s.symlink && (!directory || s.dir) && s.mode & 0o022 == 0
            })
        };
        if !valid(&at, true) {
            return false;
        }
        let parts: Vec<_> = tail.components().collect();
        for (i, part) in parts.iter().enumerate() {
            let Component::Normal(name) = part else {
                return false;
            };
            at.push(name);
            if !valid(&at, i + 1 != parts.len() || directory) {
                return false;
            }
        }
        true
    }

    pub(super) fn observe(&self, sys: &dyn System) -> RepositoryCheckoutMetadata {
        self.observe_with(&mut |path| sys.scoped_path_metadata(path))
    }

    fn observe_with(
        &self,
        read: &mut dyn FnMut(&Path) -> io::Result<crate::port::ScopedPathMetadata>,
    ) -> RepositoryCheckoutMetadata {
        let mut metadata = RepositoryCheckoutMetadata {
            shapes: Vec::new(),
            versions: Vec::new(),
        };
        let mut admission = Admission {
            home: &self.home,
            uid: self.uid,
            read,
            stats: BTreeMap::new(),
            directories: BTreeMap::new(),
        };
        let mut ancestors = BTreeSet::new();
        for (path, version) in &self.paths {
            for parent in path.ancestors().take_while(|p| p.starts_with(&self.home)) {
                ancestors.insert(parent.to_path_buf());
            }
            if *version {
                let value = admission.metadata(path).map(|m| m.version);
                metadata.versions.push((path.clone(), value));
            }
        }
        for path in ancestors {
            metadata
                .shapes
                .push((path.clone(), admission.stat(&path).map(Shape::from)));
        }
        metadata
    }

    pub(super) fn metadata(&self) -> RepositoryCheckoutMetadata {
        self.metadata.clone()
    }

    /// What differs between `before` and this later capture, as classes and never a path:
    /// `added` for a path only this one watches and `removed` for one only `before` did; for a
    /// path both watch, its class where its version or its own owner, mode or kind differs
    /// (or one of them reads its version and the other not); `shape` where a folder on the way
    /// to a watched path, not watched itself, differs in owner, mode or kind, or the two admit
    /// by different accounts. The set is empty exactly when the two captures are equal.
    pub(super) fn moved(&self, before: &Self) -> MarkMoved {
        let mut moved = MarkMoved::default();
        if self.home != before.home || self.uid != before.uid {
            moved = moved.with(MarkMoved::SHAPE);
        }
        if self
            .paths
            .keys()
            .any(|path| !before.paths.contains_key(path))
        {
            moved = moved.with(MarkMoved::ADDED);
        }
        if before
            .paths
            .keys()
            .any(|path| !self.paths.contains_key(path))
        {
            moved = moved.with(MarkMoved::REMOVED);
        }
        let both = |path: &Path| self.paths.contains_key(path) && before.paths.contains_key(path);
        let versions = |scope: &Self| -> BTreeMap<PathBuf, Result<[u8; 32], io::ErrorKind>> {
            scope.metadata.versions.iter().cloned().collect()
        };
        let (was, now) = (versions(before), versions(self));
        for path in was.keys().chain(now.keys()) {
            if both(path) && was.get(path) != now.get(path) {
                moved = moved.with(self.class(path));
            }
        }
        let was: BTreeMap<&Path, &Result<Shape, io::ErrorKind>> = before
            .metadata
            .shapes
            .iter()
            .map(|(path, shape)| (path.as_path(), shape))
            .collect();
        for (path, shape) in &self.metadata.shapes {
            if was.get(path.as_path()).is_some_and(|was| *was != shape) {
                // A watched path's own shape is its class; a folder on the way is `shape`.
                moved = moved.with(if both(path) {
                    self.class(path)
                } else {
                    MarkMoved::SHAPE
                });
            }
        }
        moved
    }

    /// The class `path` was first watched as.
    fn class(&self, path: &Path) -> MarkMoved {
        self.classes.0.get(path).copied().unwrap_or_default()
    }

    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.paths.keys().cloned().collect()
    }
}

/// Per-round component admission. A shared ancestor is observed once, while an unsafe
/// ancestor stops all deeper path reads. A later round re-admits every component afresh.
struct Admission<'a> {
    home: &'a Path,
    uid: u32,
    read: &'a mut dyn FnMut(&Path) -> io::Result<crate::port::ScopedPathMetadata>,
    stats: BTreeMap<PathBuf, Result<crate::port::ScopedPathMetadata, io::ErrorKind>>,
    directories: BTreeMap<PathBuf, bool>,
}

impl Admission<'_> {
    fn stat(&mut self, path: &Path) -> Result<crate::port::Stat, io::ErrorKind> {
        self.metadata(path).map(|m| m.stat)
    }

    fn metadata(&mut self, path: &Path) -> Result<crate::port::ScopedPathMetadata, io::ErrorKind> {
        if let Some(stat) = self.stats.get(path) {
            return *stat;
        }
        let result = if path == self.home
            || (path.starts_with(self.home) && path.parent().is_some_and(|p| self.directory(p)))
        {
            (self.read)(path).map_err(|e| e.kind())
        } else {
            Err(io::ErrorKind::PermissionDenied)
        };
        self.stats.insert(path.to_path_buf(), result);
        result
    }

    fn directory(&mut self, path: &Path) -> bool {
        if let Some(safe) = self.directories.get(path) {
            return *safe;
        }
        let safe = self
            .stat(path)
            .is_ok_and(|s| s.uid == self.uid && s.dir && !s.symlink && s.mode & 0o022 == 0);
        self.directories.insert(path.to_path_buf(), safe);
        safe
    }
}

fn normalize(base: &Path, text: &str) -> Option<PathBuf> {
    let mut path = if Path::new(text).is_absolute() {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for component in Path::new(text).components() {
        match component {
            Component::RootDir => path.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !path.pop() {
                    return None;
                }
            }
            Component::Normal(name) => path.push(name),
            Component::Prefix(_) => return None,
        }
    }
    Some(path)
}

#[cfg(test)]
#[path = "checkout_inputs_tests.rs"]
mod tests;
