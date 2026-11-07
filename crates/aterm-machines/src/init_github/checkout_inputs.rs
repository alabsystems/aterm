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
mod tests {
    use super::*;
    use crate::port::{Fake, Stat};

    fn fixture() -> (Fake, User) {
        let user = User {
            name: "fixture".into(),
            uid: 501,
            gid: 20,
            home: "/checkout-input-home".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(&user);
        directory(&sys, &user, Path::new(&user.home));
        let path = Path::new(&user.home).join(inventory::FILE);
        let bytes = br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]}]}"#;
        file(&sys, &user, &path, bytes);
        inventory::Inventory::read(&sys, &user).unwrap();
        (sys, user)
    }

    fn directory(sys: &Fake, user: &User, path: &Path) {
        // Fake's namespace listing is backed by its file entries; Stat supplies the actual
        // directory kind. Include both so the production fanout enumerator sees this entry.
        sys.file(path.to_str().unwrap(), b"").stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
    }

    fn file(sys: &Fake, user: &User, path: &Path, bytes: &[u8]) {
        sys.file(path.to_str().unwrap(), bytes).stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }

    #[test]
    fn selected_checkout_and_required_dependency_repairs_change_only_local_facts() {
        let (sys, user) = fixture();
        let home = Path::new(&user.home);
        let root = home.join("code");
        directory(&sys, &user, &root);
        let missing = Checkouts::capture(&sys, &user);
        let admin = root.join(".git");
        directory(&sys, &user, &admin);
        assert!(missing.observe(&sys) != missing.metadata());
        for path in [
            root.join("vendor"),
            root.join("vendor/library"),
            admin.join("modules"),
            admin.join("modules/library"),
            admin.join("modules/library/objects"),
            admin.join("modules/library/objects/aa"),
        ] {
            directory(&sys, &user, &path);
        }
        let dependency = root.join("vendor/library/.git");
        file(
            &sys,
            &user,
            &dependency,
            b"gitdir: ../../.git/modules/library\n",
        );
        file(
            &sys,
            &user,
            &admin.join("modules/library/objects/aa/object"),
            b"object",
        );
        let scope = Checkouts::capture(&sys, &user);
        assert!(
            scope
                .paths()
                .contains(&admin.join("modules/library/objects/aa"))
        );
        assert!(scope.paths().contains(&dependency));
        file(
            &sys,
            &user,
            &root.join("unrelated.txt"),
            b"working file changes are not readiness",
        );
        assert!(scope.observe(&sys) == scope.metadata());
        file(&sys, &user, &admin.join("config"), b"repaired origin");
        assert!(scope.observe(&sys) != scope.metadata());
        let scope = Checkouts::capture(&sys, &user);
        // The dependency's own Git configuration can be repaired without moving its
        // destination or gitfile, independently of the parent's origin repair.
        file(
            &sys,
            &user,
            &admin.join("modules/library/config"),
            b"repaired dependency origin",
        );
        assert!(scope.observe(&sys) != scope.metadata());
        assert!(sys.calls().iter().all(|call| call == "identity"));
    }

    #[test]
    fn gitfiles_cannot_enlist_unselected_or_symlinked_private_directories() {
        let (sys, user) = fixture();
        let root = Path::new(&user.home).join("code");
        for path in [&root, &root.join("vendor"), &root.join("vendor/library")] {
            directory(&sys, &user, path);
        }
        let marker = root.join("vendor/library/.git");
        for pointer in [
            "/private/app-data",
            "../../../Library/private",
            "../../.git/../../Library/private",
        ] {
            file(
                &sys,
                &user,
                &marker,
                format!("gitdir: {pointer}\n").as_bytes(),
            );
            let scope = Checkouts::capture(&sys, &user);
            assert!(scope.paths().iter().all(|path| path.starts_with(&root)));
            assert!(
                !scope
                    .paths()
                    .iter()
                    .any(|path| path.to_string_lossy().contains("app-data")
                        || path.to_string_lossy().contains("Library"))
            );
        }
        let admin = root.join(".git");
        directory(&sys, &user, &admin);
        sys.link(admin.join("modules").to_str().unwrap(), "/private/app-data");
        file(
            &sys,
            &user,
            &marker,
            b"gitdir: ../../.git/modules/library\n",
        );
        let scope = Checkouts::capture(&sys, &user);
        let before = scope.metadata();
        file(
            &sys,
            &user,
            Path::new("/private/app-data/library/config"),
            b"foreign",
        );
        assert!(scope.observe(&sys) == before);
        file(
            &sys,
            &user,
            &Path::new(&user.home).join(".config/aterm/init.toml"),
            b"[github]\nenabled=false\n",
        );
        // The canonical profile parser, rather than filesystem presence, chooses scope.
        assert!(matches!(
            profile(&sys, &user).and_then(|p| asked(p.and_then(|p| p.github), &user)),
            Ok(Asked::Off)
        ));
        assert!(Checkouts::capture(&sys, &user).paths().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn native_dependency_object_arrival_moves_cached_metadata_without_a_git_process() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().canonicalize().unwrap();
        let user = User {
            name: "fixture".into(),
            uid: std::fs::metadata(&home).unwrap().uid(),
            gid: 20,
            home: home.to_string_lossy().into_owned(),
            shell: String::new(),
            admin: false,
        };
        let private = home.join(inventory::FILE);
        let checkout = home.join("code");
        let admin = checkout.join(".git/modules/library");
        for directory in [
            private.parent().unwrap(),
            &admin.join("objects/aa"),
            &checkout.join("vendor/library"),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(&private, br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]}]}"#).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(
            checkout.join("vendor/library/.git"),
            b"gitdir: ../../.git/modules/library\n",
        )
        .unwrap();
        let sys = crate::port::Live::new();
        inventory::Inventory::read(&sys, &user).unwrap();
        let before = Checkouts::capture(&sys, &user);
        assert!(before.paths().contains(&admin.join("objects/aa")));
        std::fs::write(checkout.join("working.txt"), b"unrelated work").unwrap();
        assert!(before.observe(&sys) == before.metadata());
        // The object-storage repair producer changes an existing fanout, keeping the root,
        // gitfile, HEAD, origin, credentials and inventory unchanged.
        std::fs::write(admin.join("objects/aa/recovered"), b"object").unwrap();
        assert!(before.observe(&sys) != before.metadata());
        let repaired = Checkouts::capture(&sys, &user);
        assert!(repaired.observe(&sys) == repaired.metadata());
    }

    #[test]
    fn a_53_repository_round_observes_each_distinct_component_once() {
        let (sys, user) = fixture();
        let home = Path::new(&user.home);
        let mut repositories = Vec::new();
        for number in 0..53 {
            let name = format!("code-{number}");
            repositories.push(aterm_json::json!({"id":number+1,"name":name,"owner":{"login":"fixture"},"full_name":format!("fixture/{name}"),"private":true,"default_branch":"main","clone_url":format!("https://github.com/fixture/{name}.git"),"required_submodules":["vendor/library"]}));
            let root = home.join(name);
            for path in [
                &root,
                &root.join(".git"),
                &root.join(".git/modules"),
                &root.join(".git/modules/library"),
                &root.join(".git/modules/library/objects"),
                &root.join(".git/modules/library/objects/aa"),
                &root.join("vendor"),
                &root.join("vendor/library"),
            ] {
                directory(&sys, &user, path);
            }
            file(
                &sys,
                &user,
                &root.join("vendor/library/.git"),
                b"gitdir: ../../.git/modules/library\n",
            );
            file(
                &sys,
                &user,
                &root.join(".git/modules/library/objects/aa/object"),
                b"object",
            );
        }
        let bytes = aterm_json::to_vec(
            &aterm_json::json!({"schema":2,"account":"fixture","repositories":repositories}),
        )
        .unwrap();
        file(&sys, &user, &home.join(inventory::FILE), &bytes);
        inventory::Inventory::read(&sys, &user).unwrap();
        let scope = Checkouts::capture(&sys, &user);
        let mut calls = BTreeMap::<PathBuf, usize>::new();
        let observed = scope.observe_with(&mut |path| {
            *calls.entry(path.to_path_buf()).or_default() += 1;
            sys.scoped_path_metadata(path)
        });
        assert!(observed == scope.metadata());
        assert!(
            calls.values().all(|count| *count == 1),
            "shared ancestors must not multiply scoped metadata observations"
        );
        assert!(
            calls.len() <= scope.paths.len() + 2 * 53 + 1,
            "{} scoped observations for {} declared boundaries",
            calls.len(),
            scope.paths.len()
        );
        let blocked = home.join("code-0/.git/modules");
        sys.stat_of(
            blocked.to_str().unwrap(),
            crate::port::Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o700,
                dir: false,
                symlink: true,
                len: 0,
            },
        );
        calls.clear();
        let changed = scope.observe_with(&mut |path| {
            *calls.entry(path.to_path_buf()).or_default() += 1;
            sys.scoped_path_metadata(path)
        });
        assert!(changed != observed);
        assert!(
            !calls
                .keys()
                .any(|path| path.starts_with(blocked.join("library"))),
            "a newly linked parent stops deeper metadata reads in the next round"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_checked_parent_cannot_cross_the_scoped_metadata_boundary() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let base = scratch.path().canonicalize().unwrap();
        let home = base.join("home");
        let root = home.join("code");
        let foreign = base.join("other-application-state");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(foreign.join(".git")).unwrap();
        std::fs::create_dir(home.join(".secrets")).unwrap();
        let private = home.join(inventory::FILE);
        std::fs::write(&private, br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","private":true,"default_branch":"main","clone_url":"https://github.com/fixture/code.git"}]}"#).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(root.join(".git/config"), b"selected origin").unwrap();
        std::fs::write(foreign.join(".git/config"), b"foreign origin").unwrap();
        let user = User {
            name: "fixture".into(),
            uid: std::fs::metadata(&home).unwrap().uid(),
            gid: 20,
            home: home.to_string_lossy().into_owned(),
            shell: String::new(),
            admin: false,
        };
        let sys = crate::port::Live::new();
        let scope = Checkouts::capture(&sys, &user);
        let config = root.join(".git/config");
        assert!(
            scope
                .metadata
                .versions
                .iter()
                .any(|(path, version)| path == &config && version.is_ok())
        );
        let mut replaced = false;
        let after = scope.observe_with(&mut |path| {
            let stat = sys.scoped_path_metadata(path)?;
            if path == root && !replaced {
                std::fs::rename(&root, home.join("saved-code")).unwrap();
                std::os::unix::fs::symlink(&foreign, &root).unwrap();
                replaced = true;
            }
            Ok(stat)
        });
        assert!(replaced);
        assert!(
            after
                .versions
                .iter()
                .any(|(path, version)| path == &config && version.is_err()),
            "a parent swapped after its cached admission must not yield foreign metadata"
        );
    }
}
