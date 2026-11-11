// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An `x.py` checkout (a rustc source tree, Trust's among them) and the build tree its
//! bootstrap writes: `build/`, a real directory, or Trust's sanctioned Spotlight layout, in
//! which `build` is a link to the real sibling `build.noindex` (Trust's
//! `docs/BUILD_AND_INSTALL.md`, "Keeping Spotlight out of build/").
//!
//! The link rule is Trust's own (`src/bootstrap/src/core/build_steps/clean.rs`
//! `is_sanctioned_build_link`, and its twin in `targo-trust/src/stage2_tools.rs`): exactly that
//! one link is a build tree. Every other link (to a shared build tree, another volume, or a
//! `build.noindex` that is itself a link) and a dangling one is never followed, since it could
//! lead a reader out of the checkout. A link is read by its words first ([`named_child`]): to say
//! whether it is the build tree, only an entry directly inside the checkout is looked at, so
//! nothing on another volume, in a privacy-guarded folder or on a share that is gone is touched
//! to answer, and a link whose words say anything else is unread ([`unread_link`]): no build tree
//! and no stray `build.noindex` is claimed of where it leads. A deeper folder its words name
//! inside the checkout (`build -> out/tree`) is still bootstrap's: [`named_folder`] names it by
//! the words alone, and [`named_tree`] looks only at the real folders on that path, following no
//! link.
//! Bootstrap owns the tree and its locks: nothing here moves, links or writes it.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// The file that makes a folder an x.py checkout.
pub const XPY: &str = "x.py";

/// The build tree's name in an x.py checkout: a real directory, or the sanctioned link.
pub const BUILD: &str = "build";

/// Where the sanctioned link leads: the build tree's Spotlight-excluded home, beside `build`.
pub const BUILD_NOINDEX: &str = "build.noindex";

/// The module Trust's bootstrap is to carry once it moves a real `build/` to `build.noindex` and
/// links `build` to it at each x.py run: a checkout that holds it takes its build tree out of
/// Spotlight by itself. It is a Trust change the 2026-11-06 Continuum wave dispatched, not on
/// Trust's main that day, where the move is Trust's documented step by hand
/// (`docs/BUILD_AND_INSTALL.md`, "Keeping Spotlight out of build/"); a rename on Trust's side
/// makes [`moves_build_aside`] answer no, which says the tree stays, never that it moves.
pub const NOINDEX_LAYOUT: &str = "src/bootstrap/src/utils/trust_noindex_layout.rs";

/// Trust bootstrap's clean step. A checkout whose copy defines [`CLEAN_MARK`] supports the
/// sanctioned layout by hand: `x.py clean --all` empties the tree through the link, and the
/// `.gitignore` and the walkers that keep `build/…` working came before it (Trust's fec1353c2f2
/// and 5f31a8bcabb, 2026-09-25; `docs/BUILD_AND_INSTALL.md`, "What the repo does so that this
/// layout works"). An upstream rustc checkout's does not: its `x.py clean --all` removes only the
/// link, and its `.gitignore` leaves `build.noindex` untracked.
pub const CLEAN_STEP: &str = "src/bootstrap/src/core/build_steps/clean.rs";

/// What [`CLEAN_STEP`] defines in a checkout that supports the sanctioned layout.
pub const CLEAN_MARK: &str = "fn is_sanctioned_build_link";

/// The most of [`CLEAN_STEP`] read: Trust's is some 13 KB.
const CLEAN_MOST: u64 = 256 * 1024;

/// The most ancestors [`build_holding`] reads.
const MAX_ANCESTORS: usize = 32;

/// Whether `dir` is an x.py checkout: it holds a regular file named `x.py` (a link named `x.py`
/// makes no checkout).
#[must_use]
pub fn is_checkout(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(XPY)).is_ok_and(|m| m.is_file())
}

/// Which of an x.py checkout's build tree names, [`BUILD`] or [`BUILD_NOINDEX`], the entry
/// `name` of the folder `repo` is: that name exactly, or a spelling of it in another ASCII case
/// that the volume takes for the same entry. On a Mac's default volume, which ignores case,
/// `Build.noindex` is `build.noindex`; on a case-sensitive one it is another folder, and no
/// build tree. Only a name in another case costs a look: `repo/name` and `repo/<the name>`,
/// each by `symlink_metadata`.
#[must_use]
pub fn tree_name(repo: &Path, name: &OsStr) -> Option<&'static str> {
    let spelled = name.to_str()?;
    let tree = [BUILD, BUILD_NOINDEX]
        .into_iter()
        .find(|tree| spelled.eq_ignore_ascii_case(tree))?;
    (spelled == tree || same_entry(&repo.join(name), &repo.join(tree))).then_some(tree)
}

/// Whether `a` and `b` are one directory entry, neither followed if a link.
#[cfg(unix)]
fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether `a` and `b` are one directory entry: off Unix no inode is read, and both answering
/// is taken for one entry, as a case-insensitive volume (Windows' default) answers.
#[cfg(not(unix))]
fn same_entry(a: &Path, b: &Path) -> bool {
    std::fs::symlink_metadata(a).is_ok() && std::fs::symlink_metadata(b).is_ok()
}

/// Whether `repo/build` is the sanctioned link: a symbolic link that resolves to
/// `<canonical repo>/build.noindex`, which is a real directory, not itself a link. The link is
/// read by its words ([`named_child`]), so one spelled through another path to that folder is
/// no sanctioned link here, where Trust's realpath would sanction it: it is unread
/// ([`unread_link`]), and not followed.
#[must_use]
pub fn sanctioned_build_link(repo: &Path) -> bool {
    sanctioned_target(repo).is_some()
}

/// `<canonical repo>/build.noindex` when `repo/build` is the sanctioned link.
fn sanctioned_target(repo: &Path) -> Option<PathBuf> {
    let at = direct_child(repo)?;
    let physical = std::fs::canonicalize(repo).ok()?.join(BUILD_NOINDEX);
    // `symlink_metadata` follows nothing: a `build.noindex` that is itself a link is no
    // directory here.
    if !std::fs::symlink_metadata(&physical).is_ok_and(|m| m.file_type().is_dir()) {
        return None;
    }
    std::fs::canonicalize(&at)
        .is_ok_and(|resolved| resolved == physical)
        .then_some(physical)
}

/// The entry directly inside `repo` that the link `repo/build` names by its words alone: its
/// target joined to `repo` with `.` dropped, kept only when the target holds no `..` and the
/// entry's parent is `repo` as spelled (`build.noindex`, `./build.local`, or `repo`'s own
/// absolute spelling of one). Only the link is read. A `..` is refused rather than taken
/// lexically, because the kernel takes it after following the link before it: `hop/..` is
/// wherever `hop` leads, then up; and a bare `..` names the checkout's parent, no entry of it.
fn named_child(repo: &Path) -> Option<PathBuf> {
    let to = std::fs::read_link(repo.join(BUILD)).ok()?;
    if to.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    let at: PathBuf = repo
        .join(to)
        .components()
        .filter(|part| *part != Component::CurDir)
        .collect();
    (at.parent() == Some(repo)).then_some(at)
}

/// The real directory, not a link, that [`named_child`] names: the only entry looked at to say
/// which is the build tree.
fn direct_child(repo: &Path) -> Option<PathBuf> {
    named_child(repo).filter(|at| std::fs::symlink_metadata(at).is_ok_and(|m| m.is_dir()))
}

/// Whether `repo/build` is a link whose words name no entry directly inside `repo`
/// ([`named_child`]: a target through `..`, outside it or deeper in it), or name one that is
/// itself a link or cannot be looked at: no build tree is read where it leads, and no
/// `build.noindex` beside it is called stray. A deeper folder its words name inside `repo` is
/// still named, by [`named_folder`] and [`named_tree`], which follow no link. A link that names a
/// missing entry or a file leads nowhere, which is read.
#[must_use]
pub fn unread_link(repo: &Path) -> bool {
    if !std::fs::symlink_metadata(repo.join(BUILD)).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    named_child(repo).is_none_or(|at| match std::fs::symlink_metadata(at) {
        Ok(m) => m.file_type().is_symlink(),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    })
}

/// The real directory the x.py checkout `repo`'s bootstrap writes its build tree in: `repo/build`
/// when that is a real directory, `<canonical repo>/build.noindex` when `repo/build` is the
/// sanctioned link ([`sanctioned_build_link`]); `None` when there is no build tree, or when
/// `repo/build` is any other link, which is never followed.
#[must_use]
pub fn build_tree(repo: &Path) -> Option<PathBuf> {
    let build = repo.join(BUILD);
    if std::fs::symlink_metadata(&build).is_ok_and(|m| m.is_dir()) {
        return Some(build);
    }
    sanctioned_target(repo)
}

/// `dir` or the nearest ancestor of it, within 32 levels, that is an x.py checkout's build tree
/// by its name, with that name ([`tree_name`]): a folder named `build` or `build.noindex` whose
/// parent is an x.py checkout ([`is_checkout`]). Only names are read along the way; the files
/// looked at are each such folder's parent's `x.py`, and for a name in another case the two
/// entries [`tree_name`] compares.
#[must_use]
pub fn build_holding(dir: &Path) -> Option<(&Path, &'static str)> {
    dir.ancestors().take(MAX_ANCESTORS).find_map(|a| {
        let name = a.file_name()?;
        let repo = a.parent()?;
        if !name
            .to_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(BUILD) || n.eq_ignore_ascii_case(BUILD_NOINDEX))
            || !is_checkout(repo)
        {
            return None;
        }
        tree_name(repo, name).map(|tree| (a, tree))
    })
}

/// `repo/build.noindex`, as the volume spells it, when it is a real directory that `repo/build`
/// does not lead to: no `build`, a real one, or a link whose words name another entry or one
/// that is not there (on a case-insensitive volume `build -> Build.noindex` leads there, and
/// Trust sanctions no such link), as when the link was removed and a later x.py run wrote a
/// fresh `build/`. Nothing here reads it as the checkout's build tree. A `build.noindex` that is
/// itself a link is no tree here, and beside an unread link ([`unread_link`]) none is called
/// stray, since that link may lead to it.
#[must_use]
pub fn stray_noindex(repo: &Path) -> Option<PathBuf> {
    let noindex = repo.join(BUILD_NOINDEX);
    if !std::fs::symlink_metadata(&noindex).is_ok_and(|m| m.file_type().is_dir())
        || sanctioned_build_link(repo)
        || unread_link(repo)
    {
        return None;
    }
    let real = std::fs::canonicalize(&noindex).ok()?;
    (linked_tree(repo).as_ref() != Some(&real)).then_some(real)
}

/// The folder inside `repo`, at any depth, that the link `repo/build` names by its words alone:
/// a target of plain names (`build.local`, `out/tree`, `./out/tree`), relative, or absolute
/// under `repo`'s own spelling or its canonical one, joined to `repo` as given; `None` for no
/// link, a target with `..` in it, or an absolute one elsewhere. Only the link is read (and, for
/// an absolute target, `repo`'s own canonical path), and nothing is said of what is there.
/// Reached from `repo` by that spelling through real folders, as a walk that follows no link
/// reaches it, it is the folder the kernel resolves the link to, so a walk that meets it there
/// leaves it to bootstrap, which builds in it.
#[must_use]
pub fn named_folder(repo: &Path) -> Option<PathBuf> {
    let to = std::fs::read_link(repo.join(BUILD)).ok()?;
    let rest = if to.is_absolute() {
        match to.strip_prefix(repo) {
            Ok(rest) => rest.to_path_buf(),
            Err(_) => to
                .strip_prefix(std::fs::canonicalize(repo).ok()?)
                .ok()?
                .to_path_buf(),
        }
    } else {
        to
    };
    let mut at = repo.to_path_buf();
    let mut named = false;
    for part in rest.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => {
                at.push(name);
                named = true;
            }
            _ => return None,
        }
    }
    named.then_some(at)
}

/// [`named_folder`] with each name as the volume stores it (`build -> Out/Tree` on a Mac's
/// case-blind volume names `out/tree`), joined to `repo` as given. Each folder on the way down
/// from `repo` is looked at by `symlink_metadata` and must be a real directory, not a link, so
/// nothing is followed; only then is the path canonicalized, which resolves nothing but the
/// names' case. `None` when the folder is not there or a link lies on the way.
#[must_use]
pub fn named_tree(repo: &Path) -> Option<PathBuf> {
    let named = named_folder(repo)?;
    let mut at = repo.to_path_buf();
    for part in named.strip_prefix(repo).ok()?.components() {
        at.push(part);
        if !std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_dir()) {
            return None;
        }
    }
    let real = std::fs::canonicalize(&at).ok()?;
    let base = std::fs::canonicalize(repo).ok()?;
    Some(repo.join(real.strip_prefix(&base).ok()?))
}

/// The folder directly inside `repo` that the link `repo/build` leads to, sanctioned or not
/// (`build -> build.local`, `build -> Build.noindex`): where bootstrap builds through that link,
/// as the volume spells it. Discovery never walks it as the checkout's (bootstrap owns it and
/// its locks) and reads it as the build tree only when it is the sanctioned one
/// ([`build_tree`]). A link to anywhere else ([`unread_link`]) leads nowhere here, and is never
/// looked into.
#[must_use]
pub fn linked_tree(repo: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(direct_child(repo)?).ok()
}

/// Whether the x.py checkout `repo` supports Trust's sanctioned layout made by hand: its
/// [`CLEAN_STEP`] is a regular file whose first 256 KiB define [`CLEAN_MARK`].
#[must_use]
pub fn supports_noindex_layout(repo: &Path) -> bool {
    use std::io::Read as _;
    let path = repo.join(CLEAN_STEP);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut text = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(CLEAN_MOST).read_to_end(&mut text))
        .is_ok()
        && text
            .windows(CLEAN_MARK.len())
            .any(|w| w == CLEAN_MARK.as_bytes())
}

/// Whether the x.py checkout `repo`'s bootstrap moves its build tree to `build.noindex`: the
/// checkout holds Trust's [`NOINDEX_LAYOUT`] module as a regular file. The bootstrap of one that
/// does not (an upstream rustc checkout, Trust before that module) leaves a real `build/` where
/// it is.
#[must_use]
pub fn moves_build_aside(repo: &Path) -> bool {
    std::fs::symlink_metadata(repo.join(NOINDEX_LAYOUT)).is_ok_and(|m| m.is_file())
}

#[cfg(all(test, unix))]
#[path = "xpy_tests.rs"]
mod tests;
