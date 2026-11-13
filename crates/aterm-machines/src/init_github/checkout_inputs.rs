// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local checkout facts, derived solely from the selected private inventory.
//! These facts request another native read; they never establish Git readiness.

use super::{Asked, asked, inventory, profile, repos};
use crate::model::User;
use crate::port::System;
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

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Checkouts {
    home: PathBuf,
    uid: u32,
    paths: BTreeMap<PathBuf, bool>,
    metadata: RepositoryCheckoutMetadata,
}

impl Checkouts {
    pub(super) fn capture(sys: &dyn System, user: &User) -> Self {
        let mut scope = Self {
            home: PathBuf::from(&user.home),
            uid: user.uid,
            paths: BTreeMap::new(),
            metadata: RepositoryCheckoutMetadata {
                shapes: Vec::new(),
                versions: Vec::new(),
            },
        };
        if matches!(
            profile(sys, user).and_then(|p| asked(p.and_then(|p| p.github), user)),
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
                    scope.insert(root.join(".gitmodules"), true);
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

    fn insert(&mut self, path: PathBuf, version: bool) {
        // The private manifest is bounded to one MiB and each repository to 64 declared
        // dependencies. Never silently drop a selected destination to meet a watch limit.
        self.paths
            .entry(path)
            .and_modify(|v| *v |= version)
            .or_insert(version);
    }

    fn checkout(&mut self, sys: &dyn System, root: &Path, allowed_admin: &Path, objects: bool) {
        self.insert(root.to_path_buf(), false);
        let marker = root.join(".git");
        self.insert(marker.clone(), true);
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
        self.insert(admin.to_path_buf(), true);
        for name in [
            "config",
            "config.worktree",
            "HEAD",
            "index",
            "packed-refs",
            "shallow",
            "commondir",
            "gitdir",
            "refs",
            "refs/heads",
        ] {
            self.insert(admin.join(name), true);
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
            self.insert(path, true);
        }
        if objects {
            let object_root = admin.join("objects");
            for path in [
                &object_root,
                &object_root.join("pack"),
                &object_root.join("info"),
                &object_root.join("info/alternates"),
            ] {
                self.insert(path.clone(), true);
            }
            if self.admitted(sys, &object_root, true) {
                let names: Vec<_> = (0..=255).map(|value| format!("{value:02x}")).collect();
                if let Ok(names) = sys.scoped_child_directories(&object_root, self.uid, &names) {
                    for name in names {
                        self.insert(object_root.join(name), true);
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
