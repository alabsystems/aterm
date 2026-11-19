// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local checkout facts, derived solely from the selected private inventory.
//! These facts request another native read; they never establish Git readiness.
//!
//! A converge pass records its own Git writes to these checkouts ([`PassWrites`]) as digests of
//! each checkout's part of a capture ([`Checkouts::part`]) before and after each effect, and a
//! later capture that differs from an earlier one by those writes alone, exactly, calls for no
//! work ([`Checkouts::accounted`]).

use super::{Asked, asked, inventory, profile, repos};
use crate::model::User;
use crate::port::System;
use crate::sync::repository_refresh::{
    Accounted, CheckoutDigest, CheckoutWrites, Holder, OpenWrite, RepositoryPass,
};
use aterm_secrets::converge::MarkMoved;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::Bound::{Included, Unbounded};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const POINTER_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Each folder on the way to a watched path, and each watched path, with its shape, in path
    /// order, each path once ([`Checkouts::observe_with`] lists them from a set).
    shapes: Vec<(PathBuf, Result<Shape, io::ErrorKind>)>,
    /// Each watched path whose version is read, with its version, in path order, each path once
    /// ([`Checkouts::observe_with`] lists them from the watch's map).
    versions: Vec<(PathBuf, Result<[u8; 32], io::ErrorKind>)>,
}

impl std::fmt::Debug for RepositoryCheckoutMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryCheckoutMetadata")
            .finish_non_exhaustive()
    }
}

/// A watched path's class: which kind of Git metadata it is, to say what moved
/// ([`Checkouts::moved`]) and whether a move is one only a checkout's making makes
/// ([`Checkouts::structural`]). It is no part of what a capture observed, so it takes no part in
/// whether two captures are equal or in the digest of any part of a capture
/// ([`Checkouts::digest`]): every class equals every other, and two captures that differ only
/// here are equal.
#[derive(Clone, Copy)]
struct Class(MarkMoved);

impl PartialEq for Class {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Class {}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Checkouts {
    home: PathBuf,
    uid: u32,
    /// Each selected repository's checkout folder (`~/<name>`): every watched path lies in one.
    roots: BTreeSet<PathBuf>,
    /// Each watched path: whether its version is read, and its class.
    paths: BTreeMap<PathBuf, (bool, Class)>,
    metadata: RepositoryCheckoutMetadata,
}

/// The repositories whose checkouts are watched: the default source's private inventory, and
/// none under another source or where it cannot be read.
fn selected(sys: &dyn System, user: &User) -> Option<Vec<repos::Repo>> {
    if !matches!(
        profile(sys, user).and_then(|p| asked(p.and_then(|p| p.github))),
        Ok(Asked::Default)
    ) {
        return None;
    }
    inventory::Inventory::read(sys, user)
        .ok()
        .map(|i| i.into_repositories())
}

impl Checkouts {
    pub(super) fn capture(sys: &dyn System, user: &User) -> Self {
        Self::capture_with(
            sys,
            user,
            &selected(sys, user).unwrap_or_default(),
            &mut |path| sys.scoped_path_metadata(path),
        )
    }

    /// `repo`'s checkout alone, enlisted and read as [`Self::capture`] enlists and reads it with
    /// the rest: its [`Self::part`] is that capture's part for its folder.
    fn one(sys: &dyn System, user: &User, repo: &repos::Repo) -> Self {
        Self::capture_with(sys, user, std::slice::from_ref(repo), &mut |path| {
            sys.scoped_path_metadata(path)
        })
    }

    fn capture_with(
        sys: &dyn System,
        user: &User,
        repositories: &[repos::Repo],
        read: &mut dyn FnMut(&Path) -> io::Result<crate::port::ScopedPathMetadata>,
    ) -> Self {
        let mut scope = Self::empty(user);
        {
            let mut admission = Admission::new(Path::new(&user.home), user.uid, read);
            for repo in repositories {
                scope.enlist(sys, repo, &mut admission);
            }
        }
        // Discovery's admission ends here. Final metadata is observed afresh rather than
        // reusing the admission facts read while finding the watched paths.
        scope.metadata = scope.observe_with(read);
        scope
    }

    fn empty(user: &User) -> Self {
        Self {
            home: PathBuf::from(&user.home),
            uid: user.uid,
            roots: BTreeSet::new(),
            paths: BTreeMap::new(),
            metadata: RepositoryCheckoutMetadata {
                shapes: Vec::new(),
                versions: Vec::new(),
            },
        }
    }

    /// Watch `repo`'s checkout in `~`, with its required dependencies; never one named like a
    /// standard home folder, where no checkout is made.
    fn enlist(&mut self, sys: &dyn System, repo: &repos::Repo, admission: &mut Admission<'_>) {
        if repos::HOME_FOLDERS
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&repo.name))
        {
            return;
        }
        let root = self.home.join(&repo.name);
        let admin = root.join(".git");
        self.roots.insert(root.clone());
        if !repo.required_submodules.is_empty() {
            self.insert(root.join(".gitmodules"), true, MarkMoved::GITMODULES);
        }
        self.checkout(
            sys,
            &root,
            &admin,
            !repo.required_submodules.is_empty(),
            admission,
        );
        for relative in &repo.required_submodules {
            self.checkout(sys, &root.join(relative), &admin, true, admission);
        }
    }

    /// Watch `path`, and its version where `version`, as a path of `class`: a path keeps the
    /// first class it was given, so a checkout's `.git` is `git-dir` whether or not it is also
    /// its administrative directory.
    fn insert(&mut self, path: PathBuf, version: bool, class: MarkMoved) {
        // The private manifest is bounded to one MiB and each repository to 64 declared
        // dependencies. Never silently drop a selected destination to meet a watch limit.
        self.paths
            .entry(path)
            .and_modify(|(v, _)| *v |= version)
            .or_insert((version, Class(class)));
    }

    fn checkout(
        &mut self,
        sys: &dyn System,
        root: &Path,
        allowed_admin: &Path,
        objects: bool,
        admission: &mut Admission<'_>,
    ) {
        self.insert(root.to_path_buf(), false, MarkMoved::ROOT);
        let marker = root.join(".git");
        self.insert(marker.clone(), true, MarkMoved::GIT_DIR);
        if !admission.admitted(root) {
            return;
        }
        let Ok(metadata) = admission.metadata(&marker) else {
            return;
        };
        let stat = metadata.stat;
        if stat.uid != self.uid || stat.symlink {
            return;
        }
        let admin = if stat.dir {
            marker
        } else {
            let Some(pointer) = self.line(sys, &marker, admission) else {
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
        self.admin(sys, &admin, objects, admission);
        let common = admin.join("commondir");
        if admission.admitted(&admin)
            && let Some(pointer) = self.line(sys, &common, admission)
            && let Some(target) = normalize(&admin, &pointer)
            && target.starts_with(allowed_admin)
        {
            self.admin(sys, &target, objects, admission);
        }
    }

    fn admin(
        &mut self,
        sys: &dyn System,
        admin: &Path,
        objects: bool,
        admission: &mut Admission<'_>,
    ) {
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
        if !admission.admitted(admin) {
            return;
        }
        if let Some(head) = self.line(sys, &admin.join("HEAD"), admission)
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
            if admission.admitted(&object_root) {
                let names: Vec<_> = (0..=255).map(|value| format!("{value:02x}")).collect();
                if let Ok(names) = sys.scoped_child_directories(&object_root, self.uid, &names) {
                    for name in names {
                        self.insert(object_root.join(name), true, MarkMoved::OBJECTS_FANOUT);
                    }
                }
            }
        }
    }

    fn line(&self, sys: &dyn System, path: &Path, admission: &mut Admission<'_>) -> Option<String> {
        if !admission.admitted(path.parent()?) {
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
        let mut admission = Admission::new(&self.home, self.uid, read);
        let mut ancestors = BTreeSet::new();
        for (path, (version, _)) in &self.paths {
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
        // The class this capture first watched `path` as, where both captures watch it.
        let both = |path: &Path| {
            let (_, class) = self.paths.get(path)?;
            before.paths.contains_key(path).then_some(class.0)
        };
        // A version only one capture reads differs too.
        for (path, was, now) in by_path(&before.metadata.versions, &self.metadata.versions) {
            if was != now
                && let Some(class) = both(path)
            {
                moved = moved.with(class);
            }
        }
        for (path, was, now) in by_path(&before.metadata.shapes, &self.metadata.shapes) {
            if let (Some(was), Some(now)) = (was, now)
                && was != now
            {
                // A watched path's own shape is its class; a folder on the way is `shape`.
                moved = moved.with(both(path).unwrap_or(MarkMoved::SHAPE));
            }
        }
        moved
    }

    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.paths.keys().cloned().collect()
    }

    /// The part of this capture under `root`, as one digest: each path watched there and
    /// whether its version is read, each version read, and the owner, mode and kind of each
    /// folder there on the way to a watched path. Two parts are equal exactly when their digests
    /// are (short of a SHA-256 collision); the digest holds no path.
    fn part(&self, root: &Path) -> CheckoutDigest {
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm checkout part/1\0");
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        for (path, (versioned, _)) in self
            .paths
            .range::<Path, _>((Included(root), Unbounded))
            .take_while(|(path, _)| path.starts_with(root))
        {
            field(path.as_os_str().as_encoded_bytes());
            field(&[u8::from(*versioned)]);
        }
        for (path, version) in under(root, &self.metadata.versions) {
            field(path.as_os_str().as_encoded_bytes());
            field(format!("{version:?}").as_bytes());
        }
        for (path, shape) in under(root, &self.metadata.shapes) {
            field(path.as_os_str().as_encoded_bytes());
            field(format!("{shape:?}").as_bytes());
        }
        CheckoutDigest::new(digest.finalize())
    }

    /// Find the checkout parts whose receipt inputs changed, before computing any digest.
    /// `None` means the selection, account or a path shared outside the checkouts moved.
    fn changed_parts(&self, before: &Self) -> Option<BTreeSet<&Path>> {
        if self.home != before.home || self.uid != before.uid || self.roots != before.roots {
            return None;
        }
        let changed = changed_paths(
            before
                .paths
                .iter()
                .map(|(path, value)| (path.as_path(), value)),
            self.paths
                .iter()
                .map(|(path, value)| (path.as_path(), value)),
        )
        .chain(changed_paths(
            path_entries(&before.metadata.versions),
            path_entries(&self.metadata.versions),
        ))
        .chain(changed_paths(
            path_entries(&before.metadata.shapes),
            path_entries(&self.metadata.shapes),
        ));
        changed
            .map(|path| {
                // The roots are distinct children of home. Path order compares components,
                // so if a root owns this path it is its last root at or before the path.
                let root = self
                    .roots
                    .range::<Path, _>((Unbounded, Included(path)))
                    .next_back()?;
                path.starts_with(root).then_some(root.as_path())
            })
            .collect()
    }

    /// How the pass's own recorded writes (`writes`) account for how this later capture differs
    /// from `before`, `running` saying whether the pass that recorded last still runs
    /// ([`CheckoutWrites::running`]). [`Accounted::Own`] where every checkout whose part differs
    /// was taken from `before`'s part to this one's by those writes alone, exactly
    /// ([`CheckoutWrites::account`]), and none of them moved what only a checkout's making moves
    /// ([`Self::structural`]) unless one of the writes made it; [`Accounted::Running`] where the
    /// rest were taken to an effect that still runs; else [`Accounted::Not`], as where the home,
    /// the account, the checkouts watched or a folder on the way to them differs.
    pub(super) fn accounted(
        &self,
        before: &Self,
        writes: &CheckoutWrites,
        running: bool,
    ) -> Accounted {
        let Some(changed) = self.changed_parts(before) else {
            return Accounted::Not;
        };
        let mut accounted = Accounted::Own { made: false };
        for root in changed {
            let (from, to) = (before.part(root), self.part(root));
            if from == to {
                continue;
            }
            match writes.account(CheckoutDigest::folder(root), from, to, running) {
                Accounted::Own { made } if made || !self.structural(before, root) => {}
                Accounted::Running => accounted = Accounted::Running,
                Accounted::Own { .. } | Accounted::Not => return Accounted::Not,
            }
        }
        accounted
    }

    /// Whether the checkout at `root` moved between `before` and this capture in a way only its
    /// making moves, which no write of a pass's own on a checkout it did not make accounts for: a
    /// watched path there added or removed, but for a loose-object folder (a fetch unpacks into
    /// new ones, and the collection it may start prunes them); a folder there on the way to a
    /// watched path, the checkout's own and its `.git` among them, that came, went or changed
    /// kind, owner or mode (`root`, `shape`), but for a loose-object folder; a checkout's `.git`
    /// entry that came or went (`git-dir`); or a gitfile pointer or `.gitmodules` that changed
    /// (`pointer`, `gitmodules`).
    fn structural(&self, before: &Self, root: &Path) -> bool {
        let within = |path: &Path| path.starts_with(root);
        // The class `scope` first watched `path` as, where it watches it.
        let class =
            |scope: &Self, path: &Path| scope.paths.get(path).map(|(_, watched_as)| watched_as.0);
        let fanout =
            |scope: &Self, path: &Path| class(scope, path) == Some(MarkMoved::OBJECTS_FANOUT);
        let came_or_went = |scope: &Self, other: &Self| {
            scope.paths.iter().any(|(path, (_, watched_as))| {
                within(path)
                    && !other.paths.contains_key(path)
                    && watched_as.0 != MarkMoved::OBJECTS_FANOUT
            })
        };
        if came_or_went(self, before) || came_or_went(before, self) {
            return true;
        }
        // A shape only one capture lists differs too; each capture leaves out the shape of a
        // path it watches as a loose-object folder.
        if by_path(&before.metadata.shapes, &self.metadata.shapes).any(|(path, was, now)| {
            within(path)
                && was.filter(|_| !fanout(before, path)) != now.filter(|_| !fanout(self, path))
        }) {
            return true;
        }
        // A version both captures read, there, that differs, of a class only a checkout's making
        // moves: a `.git` entry's only where it came or went.
        by_path(&before.metadata.versions, &self.metadata.versions).any(|(path, was, now)| {
            let (Some(was), Some(now)) = (was, now) else {
                return false;
            };
            if !within(path) || was == now {
                return false;
            }
            let watched_as = class(self, path).unwrap_or_default();
            watched_as == MarkMoved::ROOT
                || watched_as == MarkMoved::POINTER
                || watched_as == MarkMoved::GITMODULES
                || (watched_as == MarkMoved::GIT_DIR && was.is_ok() != now.is_ok())
        })
    }
}

/// What one of a pass's own Git effects did, as [`PassWrites::around`] records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrote {
    /// It ran as the pass's own: what moved the checkout while it ran is its own write.
    Own,
    /// It made the checkout or a required dependency of it (a clone, an unfinished checkout's
    /// first commit, a dependency's repair): what moved includes what only a checkout's making
    /// moves.
    Made,
    /// What moved is not its own: another made what it would have.
    Not,
}

/// A converge pass's record of its own Git writes to the checkouts the repository input
/// observer watches, kept in the pass's repository record ([`RepositoryPass::begin_write`],
/// [`CheckoutWrites`]) so the observer takes none of them for new work: around each effect, the
/// checkout's part as the observer reads it ([`Checkouts::part`]) just before the effect began
/// and once it ended. Only the default source's checkouts are watched, so only they are
/// recorded. A foreign write to the same checkout while the effect runs is read with it: no
/// metadata tells the two apart.
pub struct PassWrites {
    user: User,
    repos: Vec<repos::Repo>,
    pass: Arc<RepositoryPass>,
    holder: Option<Holder>,
}

impl PassWrites {
    /// The record of `pass`'s writes to `user`'s watched checkouts: `None` where none is
    /// watched (another source than the default inventory, or an inventory that cannot be read).
    #[must_use]
    pub fn new(sys: &dyn System, user: &User, pass: Arc<RepositoryPass>) -> Option<Self> {
        let repos = selected(sys, user)?;
        let pid = std::process::id();
        Some(Self {
            user: user.clone(),
            repos,
            pass,
            holder: sys
                .process_started(pid)
                .map(|started_s| Holder { pid, started_s }),
        })
    }

    /// Run `effect`, one of the pass's own Git effects on the checkout at `folder`, and record
    /// what it wrote ([`Wrote`]) as the observer reads that checkout just before it began and
    /// once it ended. A checkout no selected repository's, or a record that cannot be written,
    /// leaves the effect's writes to call for work, as anyone's do.
    pub fn around<T>(
        &self,
        sys: &dyn System,
        folder: &Path,
        effect: impl FnOnce() -> (T, Wrote),
    ) -> T {
        let home = Path::new(&self.user.home);
        let Some((repo, root)) = self
            .repos
            .iter()
            .map(|repo| (repo, home.join(&repo.name)))
            .find(|(_, root)| root == folder)
        else {
            return effect().0;
        };
        // The folder as the capture names it, which keys the checkout's writes.
        let part = || Checkouts::one(sys, &self.user, repo).part(&root);
        let Ok(open) = self
            .pass
            .begin_write(self.holder, CheckoutDigest::folder(&root), part())
        else {
            return effect().0;
        };
        let mut ended = Ended {
            pass: self.pass.as_ref(),
            open,
            after: None,
        };
        let (value, wrote) = effect();
        ended.after = match wrote {
            Wrote::Own => Some((part(), false)),
            Wrote::Made => Some((part(), true)),
            Wrote::Not => None,
        };
        value
    }
}

/// An effect [`PassWrites::around`] recorded as begun, ended as it drops: with the checkout as
/// it was read once the effect ended, or, where its writes are not the pass's own or its thread
/// unwound, as none of the pass's.
struct Ended<'a> {
    pass: &'a RepositoryPass,
    open: OpenWrite,
    after: Option<(CheckoutDigest, bool)>,
}

impl Drop for Ended<'_> {
    fn drop(&mut self) {
        // A record that cannot be written leaves the effect's writes to call for work.
        let _ = self.pass.end_write(self.open, self.after.take());
    }
}

/// Per-observation component admission. Discovery and final metadata each have their own
/// observation: a shared ancestor is read once within it, and an unsafe ancestor stops deeper
/// reads. Neither another phase nor a later capture reuses its admissions.
struct Admission<'a> {
    home: &'a Path,
    uid: u32,
    read: &'a mut dyn FnMut(&Path) -> io::Result<crate::port::ScopedPathMetadata>,
    stats: BTreeMap<PathBuf, Result<crate::port::ScopedPathMetadata, io::ErrorKind>>,
    directories: BTreeMap<PathBuf, bool>,
}

impl<'a> Admission<'a> {
    fn new(
        home: &'a Path,
        uid: u32,
        read: &'a mut dyn FnMut(&Path) -> io::Result<crate::port::ScopedPathMetadata>,
    ) -> Self {
        Self {
            home,
            uid,
            read,
            stats: BTreeMap::new(),
            directories: BTreeMap::new(),
        }
    }

    fn admitted(&mut self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(self.home) else {
            return false;
        };
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
            && self.directory(path)
    }

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

/// Two lists as [`RepositoryCheckoutMetadata`] keeps them, each in path order with each path
/// once, paired by path in one pass over them, nothing copied: each path of either once, in
/// path order, with its entry in `was` and its entry in `now`, `None` where that list has none.
fn by_path<'a, T>(
    was: &'a [(PathBuf, T)],
    now: &'a [(PathBuf, T)],
) -> impl Iterator<Item = (&'a Path, Option<&'a T>, Option<&'a T>)> {
    debug_assert!(
        was.is_sorted_by(|a, b| a.0 < b.0) && now.is_sorted_by(|a, b| a.0 < b.0),
        "each list is in path order, each path once"
    );
    join_paths(path_entries(was), path_entries(now)).map(|(old, new)| {
        let path = old.or(new).expect("each joined row has an entry").0;
        (
            path,
            old.map(|(_, value)| value),
            new.map(|(_, value)| value),
        )
    })
}

fn path_entries<T>(entries: &[(PathBuf, T)]) -> impl Iterator<Item = (&Path, &T)> {
    entries.iter().map(|(path, value)| (path.as_path(), value))
}

/// Descendants of a root occupy one range in component order, including the root itself.
fn under<'a, T>(
    root: &'a Path,
    entries: &'a [(PathBuf, T)],
) -> impl Iterator<Item = &'a (PathBuf, T)> {
    let start = entries.partition_point(|(path, _)| path.as_path() < root);
    entries[start..]
        .iter()
        .take_while(move |(path, _)| path.starts_with(root))
}

/// The digest encodes the path's original spelling and ignores the watch class.
fn changed_paths<'a, T: Eq + 'a>(
    was: impl Iterator<Item = (&'a Path, &'a T)>,
    now: impl Iterator<Item = (&'a Path, &'a T)>,
) -> impl Iterator<Item = &'a Path> {
    join_paths(was, now).filter_map(|(old, new)| match (old, new) {
        (Some((old_path, old)), Some((new_path, new)))
            if old_path.as_os_str() == new_path.as_os_str() && old == new =>
        {
            None
        }
        (Some((path, _)), _) | (_, Some((path, _))) => Some(path),
        (None, None) => unreachable!("each joined row has an entry"),
    })
}

type PathEntry<'a, T> = (&'a Path, &'a T);
type JoinedPath<'a, T> = (Option<PathEntry<'a, T>>, Option<PathEntry<'a, T>>);

fn join_paths<'a, T: 'a>(
    was: impl Iterator<Item = PathEntry<'a, T>>,
    now: impl Iterator<Item = PathEntry<'a, T>>,
) -> impl Iterator<Item = JoinedPath<'a, T>> {
    use std::cmp::Ordering;
    let (mut was, mut now) = (was.peekable(), now.peekable());
    std::iter::from_fn(move || {
        let order = match (was.peek(), now.peek()) {
            (None, None) => return None,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some((old, _)), Some((new, _))) => old.cmp(new),
        };
        Some(match order {
            Ordering::Less => (was.next(), None),
            Ordering::Greater => (None, now.next()),
            Ordering::Equal => (was.next(), now.next()),
        })
    })
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
